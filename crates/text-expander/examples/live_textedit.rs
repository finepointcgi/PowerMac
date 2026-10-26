//! Live check of the text expander in TextEdit (macOS, needs Accessibility).
//!
//! ```text
//! cargo run -p text-expander --example live_textedit -- /path/to/scratch.txt
//! ```
//!
//! Opens the (new, empty) scratch file in TextEdit, and before every
//! synthetic key checks via Accessibility that TextEdit is frontmost with
//! that document focused; otherwise it stops. Reads the result back through
//! AX, checks the clipboard is restored after a pasted expansion, puts the
//! clipboard back as it was, and closes the document without saving.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let path = std::env::args().nth(1).expect("scratch file path");
    let code = mac::run(&path);
    std::process::exit(code);
}

#[cfg(target_os = "macos")]
#[allow(non_upper_case_globals)]
mod mac {
    use std::ffi::{c_char, c_ulong, c_void};
    use std::path::Path;
    use std::process::Command;
    use std::thread::sleep;
    use std::time::Duration;

    use text_expander::{Config, Snippet, TextExpander};

    type CFTypeRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFStringCreateWithCString(a: *const c_void, s: *const c_char, enc: u32) -> CFTypeRef;
        fn CFStringGetLength(s: CFTypeRef) -> isize;
        fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, len: isize, enc: u32) -> u8;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        static kCFBooleanTrue: CFTypeRef;
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCopyAttributeValue(el: CFTypeRef, a: CFTypeRef, out: *mut CFTypeRef) -> i32;
        fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreateKeyboardEvent(src: *mut c_void, key: u16, down: bool) -> *mut c_void;
        fn CGEventKeyboardSetUnicodeString(e: *mut c_void, len: c_ulong, s: *const u16);
        fn CGEventSetFlags(e: *mut c_void, f: u64);
        fn CGEventPost(tap: u32, e: *mut c_void);
    }

    const UTF8: u32 = 0x0800_0100;

    unsafe fn cfstr(s: &str) -> CFTypeRef {
        let c = std::ffi::CString::new(s).unwrap();
        CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8)
    }

    unsafe fn to_string(v: CFTypeRef) -> Option<String> {
        if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
            return None;
        }
        let cap = CFStringGetLength(v) * 4 + 1;
        let mut buf = vec![0 as c_char; cap as usize];
        if CFStringGetCString(v, buf.as_mut_ptr(), cap, UTF8) == 0 {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(buf.as_ptr())
                .to_string_lossy()
                .into_owned(),
        )
    }

    unsafe fn attr(el: CFTypeRef, name: &str) -> Option<CFTypeRef> {
        let n = cfstr(name);
        let mut out: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(el, n, &mut out);
        CFRelease(n);
        (err == 0 && !out.is_null()).then_some(out)
    }

    /// For TextEdit's pid, if it is frontmost: (pid, focused window title,
    /// focused element role, focused element value).
    fn focus() -> Option<(i32, String, String, String)> {
        let pid = textedit_pid()?;
        unsafe {
            let app = AXUIElementCreateApplication(pid);
            if app.is_null() {
                return None;
            }
            let front = attr(app, "AXFrontmost");
            let is_front = front.is_some_and(|f| {
                let t = f == kCFBooleanTrue;
                CFRelease(f);
                t
            });
            if !is_front {
                CFRelease(app);
                return Some((0, String::new(), String::new(), String::new()));
            }
            let win = attr(app, "AXFocusedWindow");
            let el = attr(app, "AXFocusedUIElement");
            CFRelease(app);
            let title = win
                .map(|w| {
                    let t = attr(w, "AXTitle").and_then(|t| {
                        let s = to_string(t);
                        CFRelease(t);
                        s
                    });
                    CFRelease(w);
                    t.unwrap_or_default()
                })
                .unwrap_or_default();
            let (role, value) = match el {
                Some(e) => {
                    let get = |n: &str| {
                        attr(e, n)
                            .and_then(|v| {
                                let s = to_string(v);
                                CFRelease(v);
                                s
                            })
                            .unwrap_or_default()
                    };
                    let r = (get("AXRole"), get("AXValue"));
                    CFRelease(e);
                    r
                }
                None => Default::default(),
            };
            Some((pid, title, role, value))
        }
    }

    fn textedit_pid() -> Option<i32> {
        let out = Command::new("pgrep")
            .args(["-x", "TextEdit"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()?
            .trim()
            .parse()
            .ok()
    }

    struct Poster {
        title: String,
        pid: i32,
    }

    impl Poster {
        /// Check that our document has keyboard focus.
        fn verify(&self) -> bool {
            match focus() {
                Some((pid, title, role, _))
                    if pid == self.pid && title == self.title && role == "AXTextArea" =>
                {
                    true
                }
                other => {
                    eprintln!(
                        "FOCUS LOST: {:?} - not posting",
                        other.map(|o| (o.0, o.1, o.2))
                    );
                    false
                }
            }
        }

        fn key(&self, keycode: u16, text: Option<&str>) -> bool {
            if !self.verify() {
                return false;
            }
            unsafe {
                for down in [true, false] {
                    let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, down);
                    if let Some(t) = text {
                        let u: Vec<u16> = t.encode_utf16().collect();
                        CGEventKeyboardSetUnicodeString(e, u.len() as c_ulong, u.as_ptr());
                    }
                    CGEventSetFlags(e, 0);
                    CGEventPost(0, e);
                    CFRelease(e as CFTypeRef);
                }
            }
            sleep(Duration::from_millis(25));
            true
        }

        fn typ(&self, s: &str) -> bool {
            for c in s.chars() {
                let ok = match c {
                    '\n' => self.key(0x24, None),
                    c => self.key(keycode(c), Some(&c.to_string())),
                };
                if !ok {
                    return false;
                }
            }
            true
        }

        fn backspace(&self) -> bool {
            self.key(0x33, None)
        }
    }

    fn keycode(c: char) -> u16 {
        const ANSI: &[(char, u16)] = &[
            ('a', 0x00),
            ('s', 0x01),
            ('d', 0x02),
            ('f', 0x03),
            ('h', 0x04),
            ('g', 0x05),
            ('z', 0x06),
            ('x', 0x07),
            ('c', 0x08),
            ('v', 0x09),
            ('b', 0x0B),
            ('q', 0x0C),
            ('w', 0x0D),
            ('e', 0x0E),
            ('r', 0x0F),
            ('y', 0x10),
            ('t', 0x11),
            ('o', 0x1F),
            ('u', 0x20),
            ('i', 0x22),
            ('p', 0x23),
            ('l', 0x25),
            ('j', 0x26),
            ('k', 0x28),
            ('n', 0x2D),
            ('m', 0x2E),
            (';', 0x29),
            (' ', 0x31),
            ('.', 0x2F),
            (',', 0x2B),
        ];
        let l = c.to_ascii_lowercase();
        ANSI.iter()
            .find(|(ch, _)| *ch == l)
            .map_or(0x00, |(_, k)| *k)
    }

    fn pbpaste() -> String {
        String::from_utf8_lossy(&Command::new("pbpaste").output().unwrap().stdout).into_owned()
    }

    fn close_doc(name: &str) {
        let _ = Command::new("osascript")
            .args([
                "-e",
                &format!(
                    "tell application \"TextEdit\" to close (every document whose name is \"{name}\") saving no"
                ),
            ])
            .status();
    }

    pub fn run(path: &str) -> i32 {
        let name = Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        std::fs::write(path, "").unwrap();
        // Clipboard: snapshot to put back at the end, and its text to compare.
        let clip_snapshot = keyboard_manager::clipboard::Snapshot::capture().ok();
        let clip_before = pbpaste();
        let clip_flavors = clip_snapshot.as_ref().map(|s| s.flavor_count());

        Command::new("open")
            .args(["-a", "TextEdit", path])
            .status()
            .unwrap();
        let mut poster = None;
        for _ in 0..50 {
            sleep(Duration::from_millis(200));
            if let Some(pid) = textedit_pid() {
                let p = Poster {
                    title: name.clone(),
                    pid,
                };
                if matches!(focus(), Some((fp, t, r, _)) if fp == pid && t == name && r == "AXTextArea")
                {
                    poster = Some(p);
                    break;
                }
            }
        }
        let Some(p) = poster else {
            eprintln!(
                "TextEdit document never got focus; aborting: {:?} pid {:?}",
                focus().map(|f| (f.0, f.1, f.2)),
                textedit_pid()
            );
            close_doc(&name);
            return 2;
        };

        let long: String = "0123456789".repeat(25);
        let mut cfg = Config {
            enabled: true,
            snippets: vec![
                Snippet::new(";shrug", "¯\\_(ツ)_/¯"),
                Snippet::new(";date", "{date:%Y-%m-%d}"),
                Snippet::new(";div", "<div>{cursor}</div>"),
                Snippet::new(";long", &long),
                Snippet::new(";clip", "[{clipboard}]"),
                Snippet::new(";mail", "TextEdit only"),
            ],
            ..Default::default()
        };
        let mut btw = Snippet::new("btw", "by the way");
        btw.word_boundary = true;
        btw.case_sensitive = false;
        cfg.snippets.push(btw);
        cfg.snippets[5].apps = vec!["com.apple.TextEdit".into()];
        let te = match TextExpander::start(cfg) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("start: {e:#}");
                close_doc(&name);
                return 3;
            }
        };
        sleep(Duration::from_millis(300));

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut ok = true;
        let check = |label: &str, want: &str| -> bool {
            sleep(Duration::from_millis(250));
            let got = focus().map(|f| f.3).unwrap_or_default();
            let pass = got == want;
            println!(
                "{} {label}: {:?}",
                if pass { "PASS" } else { "FAIL" },
                if pass {
                    want.to_string()
                } else {
                    format!("got {got:?}, want {want:?}")
                }
            );
            pass
        };
        let mut doc = String::new();
        type Step<'a> = (&'a str, Box<dyn Fn(&Poster) -> bool>, String);
        let steps: Vec<Step> = vec![
            (
                "last-char trigger",
                Box::new(|p: &Poster| p.typ("a;shrug")),
                "a¯\\_(ツ)_/¯".into(),
            ),
            (
                "date variable",
                Box::new(|p: &Poster| p.typ(" ;date")),
                format!(" {today}"),
            ),
            (
                "word boundary + case",
                Box::new(|p: &Poster| p.typ("\nBtw ")),
                "\nBy the way ".into(),
            ),
            (
                "not inside a word",
                Box::new(|p: &Poster| p.typ("abtw ")),
                "abtw ".into(),
            ),
            (
                "app filter",
                Box::new(|p: &Poster| p.typ("\n;mail")),
                "\nTextEdit only".into(),
            ),
        ];
        for (label, f, add) in steps {
            if !f(&p) {
                ok = false;
                break;
            }
            doc.push_str(&add);
            ok &= check(label, &doc);
        }
        // undo: "btw " then Backspace restores "btw "
        if ok && p.typ(" btw ") {
            sleep(Duration::from_millis(150));
            if p.backspace() {
                doc.push_str(" btw ");
                ok &= check("undo with backspace", &doc);
            }
        }
        // {cursor}: caret placement is checked above ("X" inside the div).
        // paste path: 250 chars through the clipboard, then restored
        if ok && p.typ("\n;long") {
            doc.push('\n');
            doc.push_str(&long);
            ok &= check("long text via paste", &doc);
            sleep(Duration::from_millis(600));
            let restored = pbpaste() == clip_before;
            println!(
                "{} clipboard restored after paste",
                if restored { "PASS" } else { "FAIL" }
            );
            ok &= restored;
        }
        if ok && p.typ("\n;clip") {
            doc.push_str(&format!("\n[{clip_before}]"));
            sleep(Duration::from_millis(250));
            let got = focus().map(|f| f.3).unwrap_or_default();
            let pass = got == doc;
            // don't print the user's clipboard
            println!("{} clipboard variable", if pass { "PASS" } else { "FAIL" });
            ok &= pass;
        }
        // {cursor}: the caret ends up inside the div, so later typing goes there
        if ok && p.typ("\n;divX") {
            doc.push_str("\n<div>X</div>");
            ok &= check("cursor", &doc);
        }
        te.stop();
        // typing after stop: no expansion (lands inside the div)
        if p.typ(" ;shrug") {
            doc.truncate(doc.len() - "</div>".len());
            doc.push_str(" ;shrug</div>");
            ok &= check("stopped: no expansion", &doc);
        }

        close_doc(&name);
        if let Some(s) = clip_snapshot {
            if pbpaste() != clip_before {
                let _ = s.restore();
            }
        }
        let after = keyboard_manager::clipboard::Snapshot::capture()
            .ok()
            .map(|s| s.flavor_count());
        let clip_ok = pbpaste() == clip_before && after == clip_flavors;
        println!(
            "{} clipboard unchanged at the end (text and {:?} flavors)",
            if clip_ok { "PASS" } else { "FAIL" },
            clip_flavors
        );
        if ok && clip_ok {
            0
        } else {
            1
        }
    }
}
