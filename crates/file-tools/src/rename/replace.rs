//! Search and replace for one name, as PowerToys' `CPowerRenameRegEx`:
//! plain or regular-expression search, case (in)sensitive, first or all
//! occurrences, a replacement with capture groups, `${…}` counters and random
//! values, and date/time variables.
//!
//! Regex flavor: PowerToys uses ECMAScript (`std::wregex`/boost). This uses
//! `fancy-regex`, which has the ECMAScript features that matter for names
//! (lookahead, lookbehind, backreferences `\1`, named groups `(?<n>…)`,
//! lazy quantifiers, `\d\w\s\b`). The replacement follows ECMAScript
//! `GetSubstitution`: `$$` `$&` `$0` `` $` `` `$'` `$1`…`$99` `$<name>`.

use chrono::{DateTime, Local};
use fancy_regex::{Captures, Regex, RegexBuilder};
use rand::Rng;

use crate::vars;

/// A piece of the replacement template.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    /// Template text: date variables, then `$` references in regex mode.
    Text(String),
    /// `${start=…,increment=…,padding=…}` counter.
    Counter {
        start: i64,
        increment: i64,
        padding: usize,
    },
    /// `${rstringalnum=N}`, `${rstringalpha=N}`, `${rstringdigit=N}`, `${ruuidv4}`.
    Random(RandomKind),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RandomKind {
    Alnum(usize),
    Alpha(usize),
    Digit(usize),
    Uuid,
}

/// Value of the first `key=<int>` in `s`.
fn opt_num(s: &str, key: &str) -> Option<i64> {
    let mut from = 0;
    while let Some(p) = s[from..].find(key) {
        let at = from + p + key.len();
        let rest = &s[at..];
        let len = rest
            .char_indices()
            .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && *c == '-'))
            .count();
        if let Ok(v) = rest[..len].parse::<i64>() {
            return Some(v);
        }
        from = at;
    }
    None
}

/// Split a replace term into text, counters and random values. `${…}` groups
/// are counters when `enumerate` is on (any content means the default
/// counter), random values when `randomize` is on and they name one
/// (random wins over counter, as in PowerToys), and literal text otherwise.
pub fn parse_template(replace: &str, enumerate: bool, randomize: bool) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut rest = replace;
    while let Some(start) = rest.find("${") {
        let Some(len) = rest[start..].find('}') else {
            break;
        };
        let group = &rest[start..start + len + 1];
        let inner = &group[2..group.len() - 1];
        let random = if randomize {
            if let Some(n) = opt_num(inner, "rstringalnum=").filter(|n| *n >= 0) {
                Some(RandomKind::Alnum(n as usize))
            } else if let Some(n) = opt_num(inner, "rstringalpha=").filter(|n| *n >= 0) {
                Some(RandomKind::Alpha(n as usize))
            } else if let Some(n) = opt_num(inner, "rstringdigit=").filter(|n| *n >= 0) {
                Some(RandomKind::Digit(n as usize))
            } else if inner.contains("ruuidv4") {
                Some(RandomKind::Uuid)
            } else {
                None
            }
        } else {
            None
        };
        text.push_str(&rest[..start]);
        let part = match random {
            Some(r) => Some(Part::Random(r)),
            None if enumerate => Some(Part::Counter {
                start: opt_num(inner, "start=").unwrap_or(0),
                increment: opt_num(inner, "increment=").unwrap_or(1),
                padding: opt_num(inner, "padding=")
                    .filter(|p| *p >= 0)
                    .unwrap_or(0)
                    .min(255) as usize,
            }),
            None => None,
        };
        match part {
            Some(p) => {
                if !text.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut text)));
                }
                parts.push(p);
            }
            None => text.push_str(group),
        }
        rest = &rest[start + group.len()..];
    }
    text.push_str(rest);
    if !text.is_empty() {
        parts.push(Part::Text(text));
    }
    parts
}

/// Whether the template has counters or random values.
pub fn has_generated(parts: &[Part]) -> bool {
    parts.iter().any(|p| !matches!(p, Part::Text(_)))
}

pub fn random_string(kind: RandomKind, rng: &mut impl Rng) -> String {
    const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const ALNUM: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    const DIGIT: &[u8] = b"0123456789";
    let (set, n) = match kind {
        RandomKind::Alnum(n) => (ALNUM, n),
        RandomKind::Alpha(n) => (ALPHA, n),
        RandomKind::Digit(n) => (DIGIT, n),
        RandomKind::Uuid => {
            let mut b: [u8; 16] = rng.gen();
            b[6] = (b[6] & 0x0f) | 0x40;
            b[8] = (b[8] & 0x3f) | 0x80;
            let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
            return format!(
                "{}-{}-{}-{}-{}",
                &h[0..8],
                &h[8..12],
                &h[12..16],
                &h[16..20],
                &h[20..32]
            );
        }
    };
    (0..n.min(255))
        .map(|_| set[rng.gen_range(0..set.len())] as char)
        .collect()
}

/// A piece of a template resolved for one item.
#[derive(Debug, Clone, PartialEq)]
pub enum Seg {
    /// Subject to `$` references (regex mode).
    Tmpl(String),
    /// Inserted as is.
    Lit(String),
}

/// Resolve counters (with `index`), random values and date variables (with
/// `time`, when given).
pub fn resolve(
    parts: &[Part],
    index: u64,
    time: Option<&DateTime<Local>>,
    rng: &mut impl Rng,
) -> Vec<Seg> {
    parts
        .iter()
        .map(|p| match p {
            Part::Text(t) => Seg::Tmpl(match time {
                Some(time) => vars::expand_date_vars(t, time),
                None => t.clone(),
            }),
            Part::Counter {
                start,
                increment,
                padding,
            } => {
                let v = start.saturating_add((index as i64).saturating_mul(*increment));
                Seg::Lit(format!("{v:0padding$}"))
            }
            Part::Random(k) => Seg::Lit(random_string(*k, rng)),
        })
        .collect()
}

/// A compiled search.
#[derive(Debug, Clone)]
pub struct Matcher {
    re: Regex,
    regex_mode: bool,
    match_all: bool,
}

impl Matcher {
    /// `None` for an empty search.
    pub fn new(
        search: &str,
        use_regex: bool,
        case_sensitive: bool,
        match_all: bool,
    ) -> Result<Option<Self>, String> {
        let search = crate::rename::nfc(search);
        if search.is_empty() {
            return Ok(None);
        }
        let pat = if use_regex {
            search
        } else {
            fancy_regex::escape(&search).into_owned()
        };
        let re = RegexBuilder::new(&pat)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Some(Self {
            re,
            regex_mode: use_regex,
            match_all,
        }))
    }

    /// Replace in `source`. Returns the result and whether anything matched,
    /// or an error (e.g. the regex backtrack limit).
    pub fn replace(&self, source: &str, segs: &[Seg]) -> Result<(String, bool), String> {
        let mut out = String::with_capacity(source.len());
        let mut last = 0;
        let mut matched = false;
        for caps in self.re.captures_iter(source) {
            let caps = caps.map_err(|e| e.to_string())?;
            let m = caps.get(0).expect("group 0");
            matched = true;
            out.push_str(&source[last..m.start()]);
            for s in segs {
                match s {
                    Seg::Lit(l) => out.push_str(l),
                    Seg::Tmpl(t) if self.regex_mode => {
                        expand_replacement(t, &caps, source, &self.re, &mut out)
                    }
                    Seg::Tmpl(t) => out.push_str(t),
                }
            }
            last = m.end();
            if !self.match_all {
                break;
            }
        }
        out.push_str(&source[last..]);
        Ok((out, matched))
    }

    /// Byte ranges of the matches in `source` (for highlighting).
    pub fn find_ranges(&self, source: &str) -> Vec<std::ops::Range<usize>> {
        let mut v = Vec::new();
        for m in self.re.find_iter(source) {
            let Ok(m) = m else { break };
            v.push(m.range());
            if !self.match_all {
                break;
            }
        }
        v
    }
}

/// ECMAScript `GetSubstitution` (plus `$0` for the whole match, as std/boost).
fn expand_replacement(t: &str, caps: &Captures, source: &str, re: &Regex, out: &mut String) {
    let m = caps.get(0).expect("group 0");
    let ngroups = caps.len() - 1;
    let b = t.as_bytes();
    let mut i = 0;
    while i < t.len() {
        if b[i] != b'$' || i + 1 >= t.len() {
            let c = t[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        let n = b[i + 1];
        match n {
            b'$' => {
                out.push('$');
                i += 2;
            }
            b'&' | b'0' => {
                out.push_str(m.as_str());
                i += 2;
            }
            b'`' => {
                out.push_str(&source[..m.start()]);
                i += 2;
            }
            b'\'' => {
                out.push_str(&source[m.end()..]);
                i += 2;
            }
            b'1'..=b'9' => {
                let d1 = (n - b'0') as usize;
                let two = b
                    .get(i + 2)
                    .filter(|c| c.is_ascii_digit())
                    .map(|c| d1 * 10 + (c - b'0') as usize);
                let (g, len) = match two {
                    Some(g) if g >= 1 && g <= ngroups => (Some(g), 3),
                    _ if d1 <= ngroups => (Some(d1), 2),
                    _ => (None, 2),
                };
                match g {
                    Some(g) => {
                        if let Some(c) = caps.get(g) {
                            out.push_str(c.as_str());
                        }
                    }
                    None => out.push_str(&t[i..i + len]),
                }
                i += len;
            }
            b'<' => {
                let has_names = re.capture_names().flatten().next().is_some();
                match t[i + 2..].find('>') {
                    Some(end) if has_names => {
                        let name = &t[i + 2..i + 2 + end];
                        if let Some(c) = caps.name(name) {
                            out.push_str(c.as_str());
                        }
                        i += 3 + end;
                    }
                    _ => {
                        out.push('$');
                        i += 1;
                    }
                }
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn rng() -> rand::rngs::StdRng {
        rand::rngs::StdRng::seed_from_u64(7)
    }

    fn rep(search: &str, replace: &str, regex: bool, cs: bool, all: bool, src: &str) -> String {
        let m = Matcher::new(search, regex, cs, all).unwrap().unwrap();
        let parts = parse_template(replace, true, true);
        let segs = resolve(&parts, 0, None, &mut rng());
        m.replace(src, &segs).unwrap().0
    }

    #[test]
    fn plain() {
        assert_eq!(rep("a", "b", false, false, true, "aAa"), "bbb");
        assert_eq!(rep("a", "b", false, true, true, "aAa"), "bAb");
        assert_eq!(rep("a", "b", false, false, false, "xaAa"), "xbAa");
        // Plain mode: no regex meta, `$1` literal.
        assert_eq!(rep("a.b", "$1", false, false, true, "a.b axb"), "$1 axb");
        assert_eq!(rep("ä", "ae", false, false, true, "Äpfel"), "aepfel");
    }

    #[test]
    fn regex_groups() {
        assert_eq!(
            rep(r"(\w+)_(\d+)", "$2-$1", true, false, true, "img_001"),
            "001-img"
        );
        assert_eq!(
            rep(r"(?<y>\d{4})", "[$<y>]", true, false, true, "a2024"),
            "a[2024]"
        );
        assert_eq!(
            rep("b", "<$&|$0|$`|$'>", true, false, true, "abc"),
            "a<b|b|a|c>c"
        );
        assert_eq!(rep("b", "$$1", true, false, true, "abc"), "a$1c");
        // $3 with only 1 group: literal; $10 with 1 group: $1 then "0".
        assert_eq!(rep("(b)", "$3", true, false, true, "abc"), "a$3c");
        assert_eq!(rep("(b)", "$10", true, false, true, "abc"), "ab0c");
        // Unmatched optional group: empty.
        assert_eq!(rep("(x)?b", "[$1]", true, false, true, "abc"), "a[]c");
        // Case-insensitive by default, first only.
        assert_eq!(rep("A", "x", true, false, false, "aaa"), "xaa");
        assert_eq!(rep("A", "x", true, true, true, "aaA"), "aax");
        // Lookaround and backreferences.
        assert_eq!(rep(r"(?<=a)b", "X", true, false, true, "abcb"), "aXcb");
        assert_eq!(rep(r"(\w)\1", "D", true, false, true, "aabcc"), "DbD");
        // Empty matches with match-all.
        assert_eq!(rep("^", "pre_", true, false, true, "name"), "pre_name");
        assert_eq!(rep("$", "_suf", true, false, true, "name"), "name_suf");
    }

    #[test]
    fn invalid_regex() {
        assert!(Matcher::new("(", true, false, true).is_err());
        assert!(Matcher::new("(", false, false, true).unwrap().is_some());
        assert!(Matcher::new("", true, false, true).unwrap().is_none());
    }

    #[test]
    fn templates() {
        assert_eq!(
            parse_template("a${start=10,increment=2,padding=4}b", true, true),
            vec![
                Part::Text("a".into()),
                Part::Counter {
                    start: 10,
                    increment: 2,
                    padding: 4
                },
                Part::Text("b".into())
            ]
        );
        assert_eq!(
            parse_template("${}", true, false),
            vec![Part::Counter {
                start: 0,
                increment: 1,
                padding: 0
            }]
        );
        assert_eq!(
            parse_template("${}", false, true),
            vec![Part::Text("${}".into())]
        );
        assert_eq!(
            parse_template("${rstringdigit=3}${ruuidv4}", true, true),
            vec![
                Part::Random(RandomKind::Digit(3)),
                Part::Random(RandomKind::Uuid)
            ]
        );
        // Randomize off: random groups are plain counters.
        assert_eq!(
            parse_template("${rstringalnum=9}", true, false),
            vec![Part::Counter {
                start: 0,
                increment: 1,
                padding: 0
            }]
        );
        assert_eq!(
            parse_template("x${start=-5}", true, true)[1],
            Part::Counter {
                start: -5,
                increment: 1,
                padding: 0
            }
        );
        assert_eq!(
            parse_template("a${b", true, true),
            vec![Part::Text("a${b".into())]
        );
    }

    #[test]
    fn counters_and_randoms() {
        let p = parse_template("${start=10,increment=2,padding=4}", true, true);
        let r = |i| match &resolve(&p, i, None, &mut rng())[0] {
            Seg::Lit(s) => s.clone(),
            _ => unreachable!(),
        };
        assert_eq!(r(0), "0010");
        assert_eq!(r(3), "0016");
        let p = parse_template("${start=-1,padding=3}", true, true);
        assert_eq!(resolve(&p, 0, None, &mut rng())[0], Seg::Lit("-01".into()));
        let mut g = rng();
        let a = random_string(RandomKind::Alnum(9), &mut g);
        assert_eq!(a.len(), 9);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        let d = random_string(RandomKind::Digit(5), &mut g);
        assert!(d.len() == 5 && d.chars().all(|c| c.is_ascii_digit()));
        let l = random_string(RandomKind::Alpha(6), &mut g);
        assert!(l.len() == 6 && l.chars().all(|c| c.is_ascii_alphabetic()));
        let u = random_string(RandomKind::Uuid, &mut g);
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_eq!(u.matches('-').count(), 4);
    }

    #[test]
    fn counter_is_literal_after_group_ref() {
        // "$1${}" must not become "$10".
        let m = Matcher::new("(a)", true, false, true).unwrap().unwrap();
        let parts = parse_template("$1${}", true, true);
        let segs = resolve(&parts, 0, None, &mut rng());
        assert_eq!(m.replace("a", &segs).unwrap().0, "a0");
    }
}
