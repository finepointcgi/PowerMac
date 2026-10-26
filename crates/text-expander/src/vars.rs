//! Replacement variables.
//!
//! | variable | becomes |
//! |---|---|
//! | `{date}` / `{date:FORMAT}` | today, `%Y-%m-%d` or a [strftime] format |
//! | `{time}` / `{time:FORMAT}` | now, `%H:%M` or a strftime format |
//! | `{clipboard}` | the clipboard's plain text |
//! | `{cursor}` | where the caret ends up (moved there with Left arrows) |
//! | `{uuid}` | a random UUID v4 |
//! | `{random:N}` / `{random:A-B}` | a random integer in 1..=N / A..=B |
//! | `{{`, `}}` | literal `{`, `}` |
//!
//! Anything else in braces is kept as typed.
//!
//! [strftime]: https://docs.rs/chrono/latest/chrono/format/strftime/index.html

use std::fmt::Write as _;

use chrono::{DateTime, Local};

/// Where variables get their values (faked in tests).
pub trait Env {
    fn now(&self) -> DateTime<Local>;
    fn clipboard(&self) -> Option<String>;
    /// Uniform in `lo..=hi`.
    fn random(&self, lo: u64, hi: u64) -> u64;
}

/// The real thing.
pub struct SystemEnv;

impl Env for SystemEnv {
    fn now(&self) -> DateTime<Local> {
        Local::now()
    }
    fn clipboard(&self) -> Option<String> {
        let mut cb = arboard::Clipboard::new().ok()?;
        keyboard_manager::clipboard::read_plain_text(&mut cb)
            .ok()
            .flatten()
    }
    fn random(&self, lo: u64, hi: u64) -> u64 {
        fastrand::u64(lo..=hi)
    }
}

/// An expanded replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    pub text: String,
    /// Char index of `{cursor}` in `text` (the first one).
    pub cursor: Option<usize>,
}

fn format_time(now: &DateTime<Local>, fmt: &str) -> Option<String> {
    let mut s = String::new();
    // An invalid format makes Display fail (instead of panicking via
    // to_string()).
    write!(s, "{}", now.format(fmt)).ok()?;
    Some(s)
}

fn uuid_v4(env: &dyn Env) -> String {
    let hi = env.random(0, u64::MAX);
    let lo = env.random(0, u64::MAX);
    let hi = (hi & 0xFFFF_FFFF_FFFF_0FFF) | 0x0000_0000_0000_4000; // version 4
    let lo = (lo & 0x3FFF_FFFF_FFFF_FFFF) | 0x8000_0000_0000_0000; // variant 10
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        hi >> 32,
        (hi >> 16) & 0xFFFF,
        hi & 0xFFFF,
        lo >> 48,
        lo & 0xFFFF_FFFF_FFFF
    )
}

fn random_var(arg: &str, env: &dyn Env) -> Option<String> {
    let arg = arg.trim();
    let (lo, hi) = match arg.split_once('-') {
        Some((a, b)) if !a.trim().is_empty() => (a.trim().parse().ok()?, b.trim().parse().ok()?),
        _ => (1, arg.parse::<u64>().ok()?),
    };
    if lo > hi {
        return None;
    }
    Some(env.random(lo, hi).to_string())
}

/// Value of one `{name[:arg]}`; `None` keeps it literal.
fn variable(name: &str, arg: Option<&str>, env: &dyn Env) -> Option<String> {
    match (name.trim(), arg) {
        ("date", a) => format_time(&env.now(), a.unwrap_or("%Y-%m-%d")),
        ("time", a) => format_time(&env.now(), a.unwrap_or("%H:%M")),
        ("clipboard", None) => Some(env.clipboard().unwrap_or_default()),
        ("uuid", None) => Some(uuid_v4(env)),
        ("random", Some(a)) => random_var(a, env),
        _ => None,
    }
}

/// Expand the variables of `template`.
pub fn expand(template: &str, env: &dyn Env) -> Expanded {
    let mut text = String::new();
    let mut cursor = None;
    let mut chars = template.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '{' if chars.peek().map(|p| p.1) == Some('{') => {
                chars.next();
                text.push('{');
            }
            '}' if chars.peek().map(|p| p.1) == Some('}') => {
                chars.next();
                text.push('}');
            }
            '{' => {
                let rest = &template[i + 1..];
                let Some(end) = rest.find('}') else {
                    text.push('{');
                    continue;
                };
                let inner = &rest[..end];
                let (name, arg) = match inner.split_once(':') {
                    Some((n, a)) => (n, Some(a)),
                    None => (inner, None),
                };
                let value = if name.trim() == "cursor" && arg.is_none() {
                    if cursor.is_none() {
                        cursor = Some(text.chars().count());
                    }
                    Some(String::new())
                } else {
                    variable(name, arg, env)
                };
                match value {
                    Some(v) => {
                        text.push_str(&v);
                        // skip the variable
                        while chars.peek().is_some_and(|&(j, _)| j <= i + 1 + end) {
                            chars.next();
                        }
                    }
                    None => text.push('{'),
                }
            }
            c => text.push(c),
        }
    }
    Expanded { text, cursor }
}

/// Text of a replacement as the settings UI previews it (variables
/// evaluated now, `{cursor}` removed).
pub fn preview(template: &str) -> String {
    expand(template, &SystemEnv).text
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::cell::Cell;

    /// Fixed time 2026-03-04 05:06:07, clipboard "CLIP", random = lo, then
    /// lo+1, ...
    pub struct FakeEnv {
        pub now: Cell<DateTime<Local>>,
        pub clip: Option<String>,
        pub n: Cell<u64>,
    }

    impl FakeEnv {
        pub fn new() -> Self {
            FakeEnv {
                now: Cell::new(Local.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap()),
                clip: Some("CLIP".into()),
                n: Cell::new(0),
            }
        }
        pub fn advance_ms(&self, ms: i64) {
            self.now
                .set(self.now.get() + chrono::Duration::milliseconds(ms));
        }
    }

    impl Env for FakeEnv {
        fn now(&self) -> DateTime<Local> {
            self.now.get()
        }
        fn clipboard(&self) -> Option<String> {
            self.clip.clone()
        }
        fn random(&self, lo: u64, hi: u64) -> u64 {
            let n = self.n.get();
            self.n.set(n + 1);
            lo.saturating_add(n).min(hi)
        }
    }

    fn x(t: &str) -> Expanded {
        expand(t, &FakeEnv::new())
    }

    #[test]
    fn dates_and_times() {
        assert_eq!(x("{date}").text, "2026-03-04");
        assert_eq!(x("{date:%d.%m.%Y}").text, "04.03.2026");
        assert_eq!(x("{time}").text, "05:06");
        assert_eq!(x("{time:%H:%M:%S}").text, "05:06:07");
        assert_eq!(
            x("on {date:%A}, {date:%B %-d}").text,
            "on Wednesday, March 4"
        );
        // invalid format: kept literally, no panic
        assert_eq!(x("{date:%Q}").text, "{date:%Q}");
    }

    #[test]
    fn clipboard_uuid_random() {
        assert_eq!(x("<{clipboard}>").text, "<CLIP>");
        let mut e = FakeEnv::new();
        e.clip = None;
        assert_eq!(expand("[{clipboard}]", &e).text, "[]");
        let u = x("{uuid}").text;
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4", "{u}");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"), "{u}");
        assert_eq!(x("{random:6}").text, "1");
        assert_eq!(x("{random:10-99}").text, "10");
        assert_eq!(x("{random:x}").text, "{random:x}");
        assert_eq!(x("{random:9-1}").text, "{random:9-1}");
        // real randomness stays in range
        for _ in 0..100 {
            let v: u64 = expand("{random:3}", &SystemEnv).text.parse().unwrap();
            assert!((1..=3).contains(&v));
        }
    }

    #[test]
    fn cursor_and_escapes() {
        assert_eq!(
            x("<p>{cursor}</p>"),
            Expanded {
                text: "<p></p>".into(),
                cursor: Some(3)
            }
        );
        assert_eq!(x("ä{cursor}ö{cursor}").cursor, Some(1));
        assert_eq!(x("{{date}}").text, "{date}");
        assert_eq!(x("a { b } {unknown} {").text, "a { b } {unknown} {");
        assert_eq!(x("fn() {{ {cursor} }}").text, "fn() {  }");
        assert_eq!(x("¯\\_(ツ)_/¯").text, "¯\\_(ツ)_/¯");
    }
}
