//! Live end-to-end check on macOS (needs Accessibility for the terminal):
//!
//! ```text
//! cargo run -p clipboard-history --example live -- <scratch dir>
//! ```
//!
//! Opens `<scratch>/kbclip-live.txt` in TextEdit and only ever sends keys
//! while that document is the focused window. Saves the clipboard first and
//! restores it at the end. Steps: clipboard watching (text, rich text,
//! concealed, image, files, dedupe), the hotkey, the picker (Down + Return
//! pastes into TextEdit), `Handle::paste_item`, and the Windows-style keys
//! preset (Home, Ctrl+Left, Ctrl+Backspace, Shift+End) plus `focused_role`.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    if let Err(e) = mac::run() {
        eprintln!("FAILED: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
mod mac {
    #![allow(non_upper_case_globals)]
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use anyhow::{bail, ensure, Context, Result};
    use clipboard_history::{ClipboardHistory, Config, Kind};
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSData, NSString};

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreateKeyboardEvent(src: *const c_void, key: u16, down: bool) -> *mut c_void;
        fn CGEventSetFlags(e: *mut c_void, f: u64);
        fn CGEventSetType(e: *mut c_void, t: u32);
        fn CGEventPost(tap: u32, e: *mut c_void);
        fn CGPreflightPostEventAccess() -> bool;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(p: *const c_void);
        fn CFStringCreateWithCString(a: *const c_void, s: *const i8, enc: u32) -> *const c_void;
        fn CFStringGetCString(s: *const c_void, buf: *mut i8, len: isize, enc: u32) -> u8;
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
        fn AXUIElementCopyAttributeValue(
            e: *const c_void,
            a: *const c_void,
            out: *mut *const c_void,
        ) -> i32;
        fn AXUIElementPerformAction(e: *const c_void, a: *const c_void) -> i32;
    }

    const CMD: u64 = 0x0010_0000 | 0x08;
    const SHIFT: u64 = 0x0002_0000 | 0x02;
    const CTRL: u64 = 0x0004_0000 | 0x01;
    const FN: u64 = 0x0080_0000;
    const NUMPAD: u64 = 0x0020_0000;
    // mac keycodes
    const K_V: u16 = 9;
    const K_A: u16 = 0;
    const K_C: u16 = 8;
    const K_S: u16 = 1;
    const K_W: u16 = 13;
    const K_X: u16 = 7;
    const K_Y: u16 = 16;
    const K_DOWN: u16 = 125;
    const K_LEFT: u16 = 123;
    const K_RETURN: u16 = 36;
    const K_DELETE: u16 = 51;
    const K_HOME: u16 = 115;
    const K_END: u16 = 119;

    fn key(code: u16, flags: u64) {
        // Modifiers go down/up as flagsChanged events first, like a keyboard.
        let mods: Vec<(u16, u64)> = [(55u16, CMD), (56, SHIFT), (59, CTRL)]
            .into_iter()
            .filter(|(_, f)| flags & f == *f)
            .collect();
        let mut held = 0u64;
        for &(kc, f) in &mods {
            held |= f;
            modifier(kc, held);
        }
        unsafe {
            for down in [true, false] {
                let e = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
                CGEventSetFlags(e, flags);
                CGEventPost(0, e);
                CFRelease(e);
            }
        }
        for &(kc, f) in mods.iter().rev() {
            held &= !f;
            modifier(kc, held);
        }
        std::thread::sleep(Duration::from_millis(60));
    }

    fn modifier(kc: u16, flags: u64) {
        unsafe {
            let e = CGEventCreateKeyboardEvent(std::ptr::null(), kc, true);
            CGEventSetType(e, 12);
            CGEventSetFlags(e, flags);
            CGEventPost(0, e);
            CFRelease(e);
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    unsafe fn ax_string(el: *const c_void, attr: &str) -> Option<String> {
        let a = std::ffi::CString::new(attr).ok()?;
        let name = CFStringCreateWithCString(std::ptr::null(), a.as_ptr(), 0x0800_0100);
        let mut out = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(el, name, &mut out);
        CFRelease(name);
        if err != 0 || out.is_null() {
            return None;
        }
        let mut buf = [0i8; 512];
        let ok = CFStringGetCString(out, buf.as_mut_ptr(), 512, 0x0800_0100) != 0;
        CFRelease(out);
        ok.then(|| {
            std::ffi::CStr::from_ptr(buf.as_ptr())
                .to_string_lossy()
                .into_owned()
        })
    }

    /// Title of the frontmost app's focused window, if TextEdit is in front.
    fn front_doc() -> Option<String> {
        let f = clipboard_history::platform::frontmost()?;
        if f.app.id.as_deref() != Some("com.apple.TextEdit") {
            return None;
        }
        unsafe {
            let app = AXUIElementCreateApplication(f.pid);
            let mut win = std::ptr::null();
            let mut err = -1;
            for attr in ["AXFocusedWindow", "AXMainWindow"] {
                let a = std::ffi::CString::new(attr).unwrap();
                let name = CFStringCreateWithCString(std::ptr::null(), a.as_ptr(), 0x0800_0100);
                err = AXUIElementCopyAttributeValue(app, name, &mut win);
                CFRelease(name);
                if err == 0 && !win.is_null() {
                    break;
                }
            }
            CFRelease(app);
            if err != 0 || win.is_null() {
                return None;
            }
            let t = ax_string(win, "AXTitle");
            CFRelease(win);
            t
        }
    }

    fn wait_for(what: &str, secs: f32, mut f: impl FnMut() -> bool) -> Result<()> {
        let t0 = Instant::now();
        while t0.elapsed().as_secs_f32() < secs {
            if f() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        bail!("timed out waiting for {what}")
    }

    /// Keys only go out while our document is the focused window.
    fn safe_key(code: u16, flags: u64) -> Result<()> {
        wait_for("the scratch document to be in front", 3.0, || {
            front_doc().is_some_and(|t| t.contains("kbclip-live"))
        })?;
        key(code, flags);
        Ok(())
    }

    fn pb_write(items: Vec<Vec<(&str, Vec<u8>)>>) {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let objs: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
            .into_iter()
            .map(|flavors| {
                let item = NSPasteboardItem::new();
                for (ty, bytes) in flavors {
                    item.setData_forType(&NSData::with_bytes(&bytes), &NSString::from_str(ty));
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        pb.writeObjects(&NSArray::from_retained_slice(&objs));
    }

    fn pb_text() -> String {
        NSPasteboard::generalPasteboard()
            .stringForType(&NSString::from_str("public.utf8-plain-text"))
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    /// Document text via Cmd+A, Cmd+C (clipboard), caret back at the end.
    fn doc_text() -> Result<String> {
        safe_key(K_A, CMD)?;
        safe_key(K_C, CMD)?;
        std::thread::sleep(Duration::from_millis(250));
        let t = pb_text();
        safe_key(K_DOWN, CMD | FN | NUMPAD)?; // caret to end
        Ok(t)
    }

    fn clear_doc() -> Result<()> {
        safe_key(K_A, CMD)?;
        safe_key(K_DELETE, 0)?;
        Ok(())
    }

    fn type_text(s: &str) -> Result<()> {
        wait_for("doc", 3.0, || {
            front_doc().is_some_and(|t| t.contains("kbclip-live"))
        })?;
        // Paste-free typing via unicode key events.
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGEventKeyboardSetUnicodeString(e: *mut c_void, len: usize, s: *const u16);
        }
        for c in s.encode_utf16() {
            unsafe {
                for down in [true, false] {
                    let e = CGEventCreateKeyboardEvent(std::ptr::null(), 0, down);
                    CGEventSetFlags(e, 0);
                    CGEventKeyboardSetUnicodeString(e, 1, &c);
                    CGEventPost(0, e);
                    CFRelease(e);
                }
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        std::thread::sleep(Duration::from_millis(100));
        Ok(())
    }

    pub fn run() -> Result<()> {
        let _ = env_logger::try_init();
        let scratch = PathBuf::from(
            std::env::args()
                .nth(1)
                .context("usage: live <scratch dir>")?,
        );
        ensure!(
            unsafe { CGPreflightPostEventAccess() },
            "no permission to post events"
        );
        std::fs::create_dir_all(&scratch)?;
        let snapshot = keyboard_manager::clipboard::Snapshot::capture()?;
        println!("saved the clipboard ({} flavors)", snapshot.flavor_count());
        let r = steps(&scratch);
        // Always restore.
        std::thread::sleep(Duration::from_millis(300));
        snapshot.restore()?;
        println!("restored the clipboard");
        r
    }

    fn steps(scratch: &std::path::Path) -> Result<()> {
        let doc = scratch.join("kbclip-live.txt");
        if !doc.exists() {
            std::fs::write(&doc, "")?;
        }
        let data = scratch.join("data");
        let _ = std::fs::remove_dir_all(&data);

        // -- watcher ---------------------------------------------------------
        let cfg = Config {
            enabled: true,
            ..Default::default()
        };
        if std::env::var_os("DEBUG_TE").is_some() {
            let f = clipboard_history::platform::frontmost();
            println!("front: {f:?}");
            if let Some(f) = f {
                unsafe {
                    let app = AXUIElementCreateApplication(f.pid);
                    for attr in ["AXFocusedWindow", "AXMainWindow", "AXFocusedUIElement"] {
                        let a = std::ffi::CString::new(attr).unwrap();
                        let name =
                            CFStringCreateWithCString(std::ptr::null(), a.as_ptr(), 0x0800_0100);
                        let mut w = std::ptr::null();
                        let err = AXUIElementCopyAttributeValue(app, name, &mut w);
                        println!(
                            "{attr}: err {err} title {:?} role {:?} subrole {:?}",
                            if w.is_null() {
                                None
                            } else {
                                ax_string(w, "AXTitle")
                            },
                            if w.is_null() {
                                None
                            } else {
                                ax_string(w, "AXRole")
                            },
                            if w.is_null() {
                                None
                            } else {
                                ax_string(w, "AXSubrole")
                            }
                        );
                    }
                }
            }
            if let Some(f) = clipboard_history::platform::frontmost() {
                unsafe { dump_sheet(f.pid) };
            }
            if std::env::var_os("DISMISS_SHEET").is_some() {
                // A sheet on our own scratch document: cancel it.
                key(53, 0);
                println!("sent Esc");
            }
            return Ok(());
        }
        if std::env::var_os("KEYS_ONLY").is_some() {
            std::process::Command::new("open")
                .arg("-a")
                .arg("TextEdit")
                .arg(&doc)
                .status()?;
            wait_for("doc", 8.0, || {
                front_doc().is_some_and(|t| t.contains("kbclip-live"))
            })?;
            std::thread::sleep(Duration::from_millis(400));
            keys_section()?;
            clear_doc()?;
            safe_key(K_S, CMD)?;
            std::thread::sleep(Duration::from_millis(300));
            safe_key(K_W, CMD)?;
            return Ok(());
        }
        let h = ClipboardHistory::start(cfg.clone(), data.clone())?;
        let (tx, rx) = mpsc::channel();
        h.on_open_picker(move || {
            let _ = tx.send(());
        });
        println!("hotkey active: {}", h.hotkey_active());
        let settle = || std::thread::sleep(Duration::from_millis(700));

        pb_write(vec![vec![(
            "public.utf8-plain-text",
            b"kbclip alpha".to_vec(),
        )]]);
        settle();
        pb_write(vec![vec![
            ("public.utf8-plain-text", b"kbclip rich".to_vec()),
            ("public.html", b"<b>kbclip rich</b>".to_vec()),
        ]]);
        settle();
        pb_write(vec![vec![
            ("public.utf8-plain-text", b"kbclip SECRET".to_vec()),
            ("org.nspasteboard.ConcealedType", vec![]),
        ]]);
        settle();
        let png = {
            let img =
                image::RgbaImage::from_fn(64, 32, |x, _| image::Rgba([x as u8 * 4, 90, 200, 255]));
            let mut v = Vec::new();
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut v), image::ImageFormat::Png)?;
            v
        };
        pb_write(vec![vec![("public.png", png)]]);
        settle();
        let f1 = scratch.join("file one.txt");
        std::fs::write(&f1, "1")?;
        let url = format!("file://{}", f1.display()).replace(' ', "%20");
        pb_write(vec![vec![
            ("public.file-url", url.into_bytes()),
            ("public.utf8-plain-text", b"file one.txt".to_vec()),
        ]]);
        settle();
        pb_write(vec![vec![(
            "public.utf8-plain-text",
            b"kbclip alpha".to_vec(),
        )]]);
        settle();
        let r = h.recent(10);
        for i in &r {
            println!("  {:?} {:?} {:?}", i.kind, i.label(40), i.source_name);
        }
        let kinds: Vec<Kind> = r.iter().map(|i| i.kind).collect();
        ensure!(
            kinds == vec![Kind::Text, Kind::Files, Kind::Image, Kind::RichText],
            "unexpected history {kinds:?}"
        );
        ensure!(
            r[0].preview == "kbclip alpha",
            "dedupe moved alpha to the top"
        );
        ensure!(
            !r.iter().any(|i| i.preview.contains("SECRET")),
            "concealed item recorded"
        );
        ensure!(r[1].preview == "file one.txt");
        ensure!(r[2].image_size == Some((64, 32)) && r[2].has_thumbnail);
        ensure!(data.join("index.json").exists());
        println!("watcher: OK");

        // Pause.
        h.pause(true);
        pb_write(vec![vec![(
            "public.utf8-plain-text",
            b"kbclip paused".to_vec(),
        )]]);
        settle();
        ensure!(
            h.recent(1)[0].preview == "kbclip alpha",
            "recorded while paused"
        );
        h.pause(false);
        println!("pause: OK");

        // -- TextEdit ---------------------------------------------------------
        std::process::Command::new("open")
            .arg("-a")
            .arg("TextEdit")
            .arg(&doc)
            .status()?;
        if let Err(e) = wait_for("TextEdit with the scratch doc", 8.0, || {
            front_doc().is_some_and(|t| t.contains("kbclip-live"))
        }) {
            bail!(
                "{e}: front is {:?} / {:?}",
                clipboard_history::platform::frontmost().map(|f| f.app),
                front_doc()
            );
        }
        std::thread::sleep(Duration::from_millis(400));
        println!(
            "focused role in TextEdit: {:?}",
            keyboard_manager::focused_role()
        );

        // Hotkey -> callback.
        safe_key(K_V, CMD | SHIFT)?;
        rx.recv_timeout(Duration::from_secs(2))
            .context("hotkey callback not called")?;
        println!("hotkey: OK");

        // Picker: Down, Return -> pastes item 2 (the file? no: pinned none;
        // order = recent) into TextEdit. Choose the rich text item instead:
        // search "rich" then Return.
        let poster = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(1500));
            let front = clipboard_history::platform::frontmost();
            let ours = front
                .as_ref()
                .is_some_and(|f| f.pid == std::process::id() as i32);
            if !ours {
                eprintln!("picker not in front ({front:?}); not typing");
                return false;
            }
            type_text_raw("rich");
            std::thread::sleep(Duration::from_millis(300));
            key(K_RETURN, 0);
            true
        });
        clipboard_history::run_picker(data.clone(), cfg.clone())?;
        ensure!(
            poster.join().unwrap_or(false),
            "picker never came to the front"
        );
        std::thread::sleep(Duration::from_millis(900));
        let t = doc_text()?;
        println!("doc after picker paste: {t:?}");
        ensure!(t.contains("kbclip rich"), "picker paste did not arrive");
        println!("picker: OK");
        ensure!(
            h.recent(1)[0].preview == "kbclip rich",
            "pasted item moved to the top"
        );

        // Tray path: paste_item.
        clear_doc()?;
        let alpha = h
            .recent(10)
            .into_iter()
            .find(|i| i.preview == "kbclip alpha")
            .unwrap();
        h.paste_item(alpha.id)?;
        std::thread::sleep(Duration::from_millis(700));
        let t = doc_text()?;
        println!("doc after paste_item: {t:?}");
        ensure!(t.trim() == "kbclip alpha", "paste_item");
        println!("paste_item: OK");
        h.stop();
        ensure!(
            !data.join("service.json").exists(),
            "endpoint removed on stop"
        );

        keys_section()?;

        // Clean up: empty the doc, save, close it.
        clear_doc()?;
        safe_key(K_S, CMD)?;
        std::thread::sleep(Duration::from_millis(300));
        safe_key(K_W, CMD)?;
        let _ = (K_X, K_Y);
        Ok(())
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFArrayGetCount(a: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(a: *const c_void, i: isize) -> *const c_void;
    }

    unsafe fn copy(el: *const c_void, attr: &str) -> *const c_void {
        let a = std::ffi::CString::new(attr).unwrap();
        let name = CFStringCreateWithCString(std::ptr::null(), a.as_ptr(), 0x0800_0100);
        let mut w = std::ptr::null();
        AXUIElementCopyAttributeValue(el, name, &mut w);
        CFRelease(name);
        w
    }

    unsafe fn dump(el: *const c_void, depth: usize) {
        if depth > 4 {
            return;
        }
        println!(
            "{}{:?} {:?} {:?}",
            "  ".repeat(depth),
            ax_string(el, "AXRole"),
            ax_string(el, "AXTitle"),
            ax_string(el, "AXValue")
        );
        let kids = copy(el, "AXChildren");
        if !kids.is_null() {
            for i in 0..CFArrayGetCount(kids) {
                dump(CFArrayGetValueAtIndex(kids, i), depth + 1);
            }
            CFRelease(kids);
        }
    }

    unsafe fn dump_sheet(pid: i32) {
        let app = AXUIElementCreateApplication(pid);
        let main = copy(app, "AXMainWindow");
        let ours = !main.is_null()
            && ax_string(main, "AXTitle").is_some_and(|t| t.contains("kbclip-live"));
        let w = copy(app, "AXFocusedWindow");
        if !w.is_null() {
            dump(w, 0);
            if ours
                && std::env::var_os("REVERT").is_some()
                && ax_string(w, "AXRole").as_deref() == Some("AXSheet")
            {
                let kids = copy(w, "AXChildren");
                for i in 0..CFArrayGetCount(kids) {
                    let k = CFArrayGetValueAtIndex(kids, i);
                    if ax_string(k, "AXTitle").as_deref() == Some("Revert") {
                        let a = std::ffi::CString::new("AXPress").unwrap();
                        let name =
                            CFStringCreateWithCString(std::ptr::null(), a.as_ptr(), 0x0800_0100);
                        println!("pressed Revert: {}", AXUIElementPerformAction(k, name));
                        CFRelease(name);
                    }
                }
                CFRelease(kids);
            }
            CFRelease(w);
        }
        CFRelease(app);
    }

    fn keys_section() -> Result<()> {
        // -- Windows-style keys -----------------------------------------------
        let km_cfg = keyboard_manager::Config::from_json(
            r#"{"pastePlainText": {"enabled": false}, "windowsStyle": {"enabled": true}}"#,
        )?;
        let km = keyboard_manager::KeyboardManager::start(km_cfg)?;
        clear_doc()?;
        type_text("abc def")?;
        safe_key(K_HOME, FN)?; // -> Cmd+Left
        type_text("X")?;
        safe_key(K_END, FN)?; // -> Cmd+Right
        safe_key(K_LEFT, CTRL | FN | NUMPAD)?; // -> Option+Left
        type_text("Y")?;
        let t = doc_text()?;
        println!("doc after Home/End/Ctrl+Left: {t:?}");
        ensure!(t.to_lowercase() == "xabc ydef", "Home / Ctrl+Left: {t:?}");
        safe_key(K_DELETE, CTRL)?; // Ctrl+Backspace at the end -> Option+Backspace
        let t = doc_text()?;
        println!("doc after Ctrl+Backspace: {t:?}");
        ensure!(t.to_lowercase() == "xabc ", "Ctrl+Backspace: {t:?}");
        // Shift+Home selects the line; typing replaces it.
        safe_key(K_HOME, SHIFT | FN)?;
        type_text("Z")?;
        let t = doc_text()?;
        println!("doc after Shift+Home: {t:?}");
        ensure!(t.to_lowercase() == "z", "Shift+Home: {t:?}");
        km.stop();
        println!("windows-style keys: OK");

        Ok(())
    }

    /// Typing into the picker (our own window).
    fn type_text_raw(s: &str) {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGEventKeyboardSetUnicodeString(e: *mut c_void, len: usize, s: *const u16);
        }
        for c in s.encode_utf16() {
            unsafe {
                for down in [true, false] {
                    let e = CGEventCreateKeyboardEvent(std::ptr::null(), 0, down);
                    CGEventSetFlags(e, 0);
                    CGEventKeyboardSetUnicodeString(e, 1, &c);
                    CGEventPost(0, e);
                    CFRelease(e);
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}
