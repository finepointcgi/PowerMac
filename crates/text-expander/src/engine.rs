//! The platform-independent part: the rolling buffer of typed characters,
//! trigger matching, and the action plan (n backspaces + text) for an
//! expansion or its undo.
//!
//! The backends feed every physical key-down as a [`Key`] and act on the
//! [`Decision`]: `Pass` lets the event through, `Swallow(plan)` drops it and
//! performs the plan. The key that completes a trigger (its last
//! character, or the space/punctuation after a word-boundary trigger) is
//! swallowed, so only the characters already on screen are deleted.

use std::sync::Arc;

use chrono::{DateTime, Local};

use crate::config::{Config, Snippet, TERMINALS};
use crate::vars::{self, Env};

/// Longest trigger, in characters.
pub const MAX_TRIGGER: usize = 64;
/// How long after an expansion Backspace still undoes it.
pub const UNDO_WINDOW_MS: i64 = 10_000;

/// A physical key-down, as the backend sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// Text typed by an ordinary key (no Cmd/Ctrl/Win held). Return is
    /// `"\n"`, Tab `"\t"`.
    Text(String),
    /// Backspace (no modifiers).
    Backspace,
    /// Anything that moves the caret or changes context: arrows, Home/End,
    /// Escape, forward delete, shortcuts with Cmd/Ctrl/Win, mouse clicks,
    /// focus changes, secure input.
    Reset,
}

/// How the replacement gets into the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Insert {
    /// Unicode key events (`\n` presses Return, `\t` Tab).
    Type(String),
    /// Through the clipboard (saved and restored).
    Paste(String),
}

impl Insert {
    pub fn text(&self) -> &str {
        match self {
            Insert::Type(t) | Insert::Paste(t) => t,
        }
    }
}

/// What to do, in this order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Right-arrow presses first (undo of a `{cursor}` expansion).
    pub right: usize,
    pub backspaces: usize,
    pub insert: Insert,
    /// Left-arrow presses after inserting (`{cursor}`).
    pub left: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Pass,
    Swallow(Plan),
}

/// Number of user-perceived characters (what one Backspace deletes), close
/// enough to grapheme clusters for typed text: combining marks, variation
/// selectors, skin tones and ZWJ sequences join the previous character,
/// regional-indicator pairs (flags) and CR LF count once.
pub fn grapheme_count(s: &str) -> usize {
    let mut n = 0;
    let mut prev: Option<char> = None;
    let mut join_next = false;
    let mut ri_open = false;
    for c in s.chars() {
        let u = c as u32;
        let extends = matches!(u,
            0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF
            | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F
            | 0xE0100..=0xE01EF | 0x200D);
        let is_ri = (0x1F1E6..=0x1F1FF).contains(&u);
        let crlf = c == '\n' && prev == Some('\r');
        if prev.is_none() || !(extends || join_next || crlf || (is_ri && ri_open)) {
            n += 1;
            ri_open = is_ri;
        } else if is_ri {
            ri_open = false;
        }
        join_next = u == 0x200D;
        prev = Some(c);
    }
    n
}

/// Characters that end a word (for word-boundary triggers).
pub fn is_separator(c: char) -> bool {
    c.is_whitespace() || (c.is_ascii_punctuation() && c != '_')
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Normalize an app identifier: lower-case file name without `.app`/`.exe`.
pub fn normalize_app(s: &str) -> String {
    keyboard_manager::engine::normalize_app(s)
}

#[derive(Debug, Clone)]
struct CompiledSnippet {
    trigger: Vec<char>,
    snippet: Snippet,
    apps: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct Compiled {
    enabled: bool,
    snippets: Vec<CompiledSnippet>,
    excluded: Vec<String>,
    undo: bool,
    paste_threshold: usize,
}

fn compile(cfg: &Config) -> Compiled {
    let mut snippets: Vec<CompiledSnippet> = cfg
        .snippets
        .iter()
        .filter(|s| s.enabled && !s.trigger.is_empty())
        .filter(|s| s.trigger.chars().count() <= MAX_TRIGGER)
        .map(|s| CompiledSnippet {
            trigger: s.trigger.chars().collect(),
            snippet: s.clone(),
            apps: s
                .apps
                .iter()
                .map(|a| normalize_app(a))
                .filter(|a| !a.is_empty())
                .collect(),
        })
        .collect();
    // Longest trigger first; app-specific before global; then config order.
    snippets.sort_by_key(|s| (std::cmp::Reverse(s.trigger.len()), s.apps.is_empty()));
    let mut excluded: Vec<String> = cfg.excluded_apps.iter().map(|a| normalize_app(a)).collect();
    if cfg.exclude_terminals {
        excluded.extend(TERMINALS.iter().map(|a| normalize_app(a)));
    }
    excluded.retain(|a| !a.is_empty());
    Compiled {
        enabled: cfg.enabled,
        snippets,
        excluded,
        undo: cfg.undo_with_backspace,
        paste_threshold: cfg.paste_threshold,
    }
}

#[derive(Debug, Clone)]
struct Undo {
    at: DateTime<Local>,
    /// What the user typed (trigger + the swallowed key).
    original: String,
    /// Characters the expansion put on screen.
    inserted: usize,
    /// Characters after the caret (cursor placement).
    after_caret: usize,
}

/// A match found in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub index: usize,
    /// The trigger as typed (its case may differ).
    pub typed: String,
    /// The separator that completed a word-boundary trigger.
    pub separator: Option<String>,
}

/// The expander state machine. Cheap to clone the config into.
#[derive(Debug, Clone, Default)]
pub struct Expander {
    c: Arc<Compiled>,
    buf: Vec<char>,
    undo: Option<Undo>,
}

/// Case-carrying for case-insensitive triggers.
fn carry_case(typed: &str, trigger: &str, text: String) -> String {
    let letters: Vec<char> = typed.chars().filter(|c| c.is_alphabetic()).collect();
    let trig_letters: Vec<char> = trigger.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() || letters == trig_letters {
        return text;
    }
    if letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase()) {
        return text.to_uppercase();
    }
    if letters[0].is_uppercase() && !trig_letters.first().is_some_and(|c| c.is_uppercase()) {
        let mut cs = text.chars();
        return match cs.next() {
            Some(f) => f.to_uppercase().chain(cs).collect(),
            None => text,
        };
    }
    text
}

impl Expander {
    pub fn new(cfg: &Config) -> Self {
        Expander {
            c: Arc::new(compile(cfg)),
            buf: Vec::new(),
            undo: None,
        }
    }

    /// New config; the buffer is kept.
    pub fn set_config(&mut self, cfg: &Config) {
        self.c = Arc::new(compile(cfg));
        self.undo = None;
    }

    /// The typed characters remembered right now.
    pub fn buffer(&self) -> String {
        self.buf.iter().collect()
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.undo = None;
    }

    /// Whether nothing may expand in an app with these identifiers.
    pub fn excluded(&self, app: &[String]) -> bool {
        app.iter()
            .any(|a| self.c.excluded.contains(&normalize_app(a)))
    }

    fn buffer_cap(&self) -> usize {
        self.c
            .snippets
            .iter()
            .map(|s| s.trigger.len() + 2)
            .max()
            .unwrap_or(0)
            .min(MAX_TRIGGER + 2)
    }

    /// Find a trigger ending the buffer (which already contains the key
    /// just typed, `typed_now` characters long).
    pub fn find(&self, app: &mut dyn FnMut() -> Vec<String>) -> Option<Match> {
        let last = *self.buf.last()?;
        let mut ids: Option<Vec<String>> = None;
        for (i, s) in self.c.snippets.iter().enumerate() {
            let (body, sep) = if s.snippet.word_boundary {
                if !is_separator(last) {
                    continue;
                }
                (&self.buf[..self.buf.len() - 1], Some(last))
            } else {
                (&self.buf[..], None)
            };
            let t = &s.trigger;
            if body.len() < t.len() {
                continue;
            }
            let tail = &body[body.len() - t.len()..];
            let eq = if s.snippet.case_sensitive {
                tail == &t[..]
            } else {
                tail.iter().zip(t).all(|(a, b)| lower(*a) == lower(*b))
            };
            if !eq {
                continue;
            }
            if s.snippet.word_boundary {
                // The trigger must start a word (or the buffer).
                let before = body.len() - t.len();
                if before > 0 && !is_separator(body[before - 1]) {
                    continue;
                }
            }
            if !s.apps.is_empty() {
                let ids =
                    ids.get_or_insert_with(|| app().iter().map(|a| normalize_app(a)).collect());
                if !s.apps.iter().any(|a| ids.contains(a)) {
                    continue;
                }
            }
            return Some(Match {
                index: i,
                typed: tail.iter().collect(),
                separator: sep.map(String::from),
            });
        }
        None
    }

    /// Process one key-down. `app` returns the frontmost app's identifiers
    /// (called lazily, only for app-scoped snippets).
    pub fn key(
        &mut self,
        key: Key,
        app: &mut dyn FnMut() -> Vec<String>,
        env: &dyn Env,
    ) -> Decision {
        if !self.c.enabled {
            return Decision::Pass;
        }
        let undo = self.undo.take();
        match key {
            Key::Reset => {
                self.buf.clear();
                Decision::Pass
            }
            Key::Backspace => {
                if let Some(u) = undo {
                    let age = env.now().signed_duration_since(u.at).num_milliseconds();
                    if self.c.undo && (0..=UNDO_WINDOW_MS).contains(&age) {
                        self.buf.clear();
                        return Decision::Swallow(Plan {
                            right: u.after_caret,
                            backspaces: u.inserted,
                            insert: Insert::Type(u.original),
                            left: 0,
                        });
                    }
                }
                self.buf.pop();
                Decision::Pass
            }
            Key::Text(s) => {
                if s.is_empty() {
                    return Decision::Pass; // dead key
                }
                if s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
                    self.buf.clear();
                    return Decision::Pass;
                }
                let typed_now = s.chars().count();
                self.buf.extend(s.chars());
                let cap = self.buffer_cap();
                if self.buf.len() > cap {
                    self.buf.drain(..self.buf.len() - cap);
                }
                let Some(m) = self.find(app) else {
                    return Decision::Pass;
                };
                let plan = self.plan(&m, typed_now, env);
                self.buf.clear();
                Decision::Swallow(plan)
            }
        }
    }

    /// The plan for a match; remembers what's needed for undo.
    fn plan(&mut self, m: &Match, typed_now: usize, env: &dyn Env) -> Plan {
        let s = &self.c.snippets[m.index];
        let x = vars::expand(&s.snippet.replacement, env);
        let mut text = if s.snippet.case_sensitive {
            x.text
        } else {
            carry_case(&m.typed, &s.snippet.trigger, x.text)
        };
        let sep = m.separator.clone().unwrap_or_default();
        // Characters of this key that belong to the trigger (not yet on
        // screen, since the key is swallowed).
        let trigger_chars = m.typed.chars().count();
        let in_key = if m.separator.is_some() {
            typed_now.saturating_sub(1).min(trigger_chars)
        } else {
            typed_now.min(trigger_chars)
        };
        let on_screen: String = m.typed.chars().take(trigger_chars - in_key).collect();
        let backspaces = grapheme_count(&on_screen);
        let after_caret = match x.cursor {
            Some(i) => {
                grapheme_count(&text.chars().skip(i).collect::<String>()) + grapheme_count(&sep)
            }
            None => 0,
        };
        text.push_str(&sep);
        let inserted = grapheme_count(&text);
        self.undo = Some(Undo {
            at: env.now(),
            original: format!("{}{}", m.typed, sep),
            inserted,
            after_caret,
        });
        let insert = if s.snippet.paste || inserted > self.c.paste_threshold {
            Insert::Paste(text)
        } else {
            Insert::Type(text)
        };
        Plan {
            right: 0,
            backspaces,
            insert,
            left: after_caret,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vars::tests::FakeEnv;

    fn cfg(snips: Vec<Snippet>) -> Config {
        Config {
            enabled: true,
            snippets: snips,
            ..Default::default()
        }
    }

    struct T {
        e: Expander,
        env: FakeEnv,
        app: Vec<String>,
        queries: usize,
    }

    impl T {
        fn new(snips: Vec<Snippet>) -> T {
            T::with(cfg(snips))
        }
        fn with(c: Config) -> T {
            T {
                e: Expander::new(&c),
                env: FakeEnv::new(),
                app: vec!["com.apple.TextEdit".into()],
                queries: 0,
            }
        }
        fn key(&mut self, k: Key) -> Decision {
            let app = self.app.clone();
            let q = &mut self.queries;
            self.e.key(
                k,
                &mut || {
                    *q += 1;
                    app.clone()
                },
                &self.env,
            )
        }
        /// Type each char; return the decisions that weren't Pass.
        fn typ(&mut self, s: &str) -> Vec<(usize, Plan)> {
            let mut out = vec![];
            for (i, c) in s.chars().enumerate() {
                if let Decision::Swallow(p) = self.key(Key::Text(c.to_string())) {
                    out.push((i, p));
                }
            }
            out
        }
    }

    fn snip(t: &str, r: &str) -> Snippet {
        Snippet::new(t, r)
    }

    fn plan(bs: usize, text: &str) -> Plan {
        Plan {
            right: 0,
            backspaces: bs,
            insert: Insert::Type(text.into()),
            left: 0,
        }
    }

    #[test]
    fn expands_on_last_char() {
        let mut t = T::new(vec![snip(";addr", "1 Infinite Loop")]);
        let r = t.typ("my ;addr");
        // the final "r" is swallowed; ";add" (4 chars) is deleted
        assert_eq!(r, vec![(7, plan(4, "1 Infinite Loop"))]);
        assert_eq!(t.e.buffer(), "");
        // typing on doesn't re-trigger
        assert!(t.typ(" and more").is_empty());
    }

    #[test]
    fn disabled_does_nothing() {
        let mut c = cfg(vec![snip(";a", "x")]);
        c.enabled = false;
        let mut t = T::with(c);
        assert!(t.typ(";a").is_empty());
        let mut c = cfg(vec![snip(";a", "x")]);
        c.snippets[0].enabled = false;
        let mut t = T::with(c);
        assert!(t.typ(";a").is_empty());
    }

    #[test]
    fn buffer_handling() {
        let mut t = T::new(vec![snip(";addr", "X")]);
        // backspace edits the buffer
        assert!(t.typ(";adx").is_empty());
        assert_eq!(t.key(Key::Backspace), Decision::Pass);
        assert_eq!(t.typ("dr").len(), 1);
        // a reset (click, arrow, shortcut, focus change) forgets
        t.typ(";ad");
        t.key(Key::Reset);
        assert!(t.typ("dr").is_empty());
        // dead key (empty text) doesn't disturb
        t.typ(";ad");
        t.key(Key::Text(String::new()));
        assert_eq!(t.typ("dr").len(), 1);
        // control characters (e.g. Escape's \u{1b}) reset
        t.typ(";ad");
        t.key(Key::Text("\u{1b}".into()));
        assert!(t.typ("dr").is_empty());
        // the buffer stays short
        t.typ(&"x".repeat(500));
        assert!(t.e.buffer().len() <= 7, "{}", t.e.buffer());
        assert_eq!(t.typ(";addr").len(), 1);
    }

    #[test]
    fn longest_trigger_wins_and_overlaps() {
        let mut t = T::new(vec![snip("dr", "SHORT"), snip(";addr", "LONG")]);
        assert_eq!(t.typ(";addr"), vec![(4, plan(4, "LONG"))]);
        assert_eq!(t.typ("xdr"), vec![(2, plan(1, "SHORT"))]);
    }

    #[test]
    fn case_sensitivity() {
        let mut t = T::new(vec![snip(";sig", "best regards")]);
        assert!(t.typ(";SIG").is_empty(), "case-sensitive by default");
        let mut s = snip(";sig", "best regards");
        s.case_sensitive = false;
        let mut t = T::new(vec![s]);
        assert_eq!(t.typ(";sig")[0].1, plan(3, "best regards"));
        assert_eq!(t.typ(";Sig")[0].1, plan(3, "Best regards"));
        assert_eq!(t.typ(";SIG")[0].1, plan(3, "BEST REGARDS"));
        assert_eq!(t.typ(";sIg")[0].1, plan(3, "best regards"));
    }

    #[test]
    fn word_boundary() {
        let mut s = snip("btw", "by the way");
        s.word_boundary = true;
        let mut t = T::new(vec![s]);
        // not on the last char, but on the separator, which is kept
        assert!(t.typ("btw").is_empty());
        let r = t.key(Key::Text(" ".into()));
        assert_eq!(r, Decision::Swallow(plan(3, "by the way ")));
        // punctuation and Return also end a word
        assert_eq!(t.typ("btw,"), vec![(3, plan(3, "by the way,"))]);
        assert_eq!(t.typ("btw\n"), vec![(3, plan(3, "by the way\n"))]);
        // inside a word: no
        assert!(t.typ("abtw ").is_empty());
        assert!(t.typ("btwx ").is_empty());
        // after a separator: yes
        assert_eq!(t.typ("(btw)"), vec![(4, plan(3, "by the way)"))]);
    }

    #[test]
    fn app_filter_and_exclusions() {
        let mut s = snip(";m", "mail only");
        s.apps = vec!["com.apple.mail".into()];
        let mut t = T::new(vec![s, snip(";g", "global")]);
        assert!(t.typ(";m").is_empty());
        assert_eq!(t.queries, 1);
        t.app = vec!["com.apple.Mail".into(), "Mail".into()];
        assert_eq!(t.typ(";m").len(), 1);
        let q = t.queries;
        assert_eq!(t.typ(";g").len(), 1);
        assert_eq!(t.queries, q, "global snippets don't query the app");
        assert!(t.e.excluded(&["com.1password.1password".into()]));
        assert!(t
            .e
            .excluded(&["C:\\Program Files\\KeePass\\KeePass.exe".into()]));
        assert!(!t.e.excluded(&["com.apple.Terminal".into()]));
        let mut c = cfg(vec![]);
        c.exclude_terminals = true;
        assert!(Expander::new(&c).excluded(&["com.apple.Terminal".into()]));
    }

    #[test]
    fn variables_cursor_and_graphemes() {
        let mut t = T::new(vec![
            snip(";d", "{date} {time:%H:%M:%S}"),
            snip(";p", "<p>{cursor}</p>"),
            snip(";e", "👍🏽 🇩🇪 e\u{301}"),
        ]);
        assert_eq!(t.typ(";d")[0].1, plan(1, "2026-03-04 05:06:07"));
        assert_eq!(
            t.typ(";p")[0].1,
            Plan {
                right: 0,
                backspaces: 1,
                insert: Insert::Type("<p></p>".into()),
                left: 4
            }
        );
        // undo of the emoji expansion deletes 5 user-perceived chars
        t.typ(";e");
        assert_eq!(
            t.key(Key::Backspace),
            Decision::Swallow(Plan {
                right: 0,
                backspaces: 5,
                insert: Insert::Type(";e".into()),
                left: 0
            })
        );
        assert_eq!(grapheme_count("👨‍👩‍👧"), 1);
        assert_eq!(grapheme_count("a\r\nb"), 3);
        assert_eq!(grapheme_count("🇩🇪🇫🇷"), 2);
        assert_eq!(grapheme_count("¯\\_(ツ)_/¯"), 9);
    }

    #[test]
    fn undo_with_backspace() {
        let mut s = snip("teh", "the");
        s.word_boundary = true;
        let mut t = T::new(vec![s, snip(";p", "<b>{cursor}</b>")]);
        assert_eq!(t.typ("teh ").len(), 1);
        t.env.advance_ms(500);
        assert_eq!(
            t.key(Key::Backspace),
            Decision::Swallow(Plan {
                right: 0,
                backspaces: 4,
                insert: Insert::Type("teh ".into()),
                left: 0
            })
        );
        // the next Backspace is an ordinary one
        assert_eq!(t.key(Key::Backspace), Decision::Pass);
        // cursor: move right over the rest first
        t.typ(";p");
        assert_eq!(
            t.key(Key::Backspace),
            Decision::Swallow(Plan {
                right: 4,
                backspaces: 7,
                insert: Insert::Type(";p".into()),
                left: 0
            })
        );
        // any other key in between: no undo
        t.typ("teh ");
        t.typ("x");
        assert_eq!(t.key(Key::Backspace), Decision::Pass);
        // too late
        t.typ("teh ");
        t.env.advance_ms(UNDO_WINDOW_MS + 1);
        assert_eq!(t.key(Key::Backspace), Decision::Pass);
        // switched off
        let mut c = cfg(vec![snip(";a", "b")]);
        c.undo_with_backspace = false;
        let mut t = T::with(c);
        t.typ(";a");
        assert_eq!(t.key(Key::Backspace), Decision::Pass);
    }

    #[test]
    fn long_text_is_pasted() {
        let long = "x".repeat(300);
        let mut s = snip(";short", "abc");
        s.paste = true;
        let mut t = T::new(vec![snip(";long", &long), s]);
        assert_eq!(t.typ(";long")[0].1.insert, Insert::Paste(long));
        assert_eq!(t.typ(";short")[0].1.insert, Insert::Paste("abc".into()));
    }

    #[test]
    fn multi_char_key_text() {
        // An input method / compose sequence delivering two chars at once.
        let mut t = T::new(vec![snip(";né", "born")]);
        t.typ(";n");
        assert_eq!(
            t.key(Key::Text("é".into())),
            Decision::Swallow(plan(2, "born"))
        );
        t.typ(";");
        assert_eq!(
            t.key(Key::Text("né".into())),
            Decision::Swallow(plan(1, "born"))
        );
    }
}
