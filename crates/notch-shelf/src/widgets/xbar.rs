//! xbar / SwiftBar / BitBar plugin support: the refresh interval in the
//! file name and a parser for the plugin output format.
//!
//! ```text
//! Title line(s)            (several: they cycle)
//! ---
//! Item | href=https://… color=red sfimage=star.fill
//! Run me | bash=/path/script param1=a param2="b c" terminal=false refresh=true
//! --Submenu item           (one "--" per level)
//! -----                    (separator inside a submenu)
//! ---                      (separator)
//! ```

use std::path::{Path, PathBuf};

use super::content::{parse_color, strip_ansi, Content, ImageData, Item, ItemAction, Style};

/// Refresh interval from a plugin file name: `cpu.10s.sh`, `wx.5m.py`,
/// `x.1h.rb`, `y.1d.js`, also `ms` (`fast.500ms.sh`). `None` without one.
pub fn interval_from_name(name: &str) -> Option<f64> {
    let parts: Vec<&str> = name.split('.').collect();
    // name . interval . ext  (the interval is the second-to-last part, or
    // the last when there is no extension).
    let cands = if parts.len() >= 3 { vec![parts[parts.len() - 2]] } else if parts.len() == 2 { vec![parts[1]] } else { vec![] };
    for c in cands {
        let c = c.trim();
        let (num, mult) = if let Some(n) = c.strip_suffix("ms") {
            (n, 0.001)
        } else if let Some(n) = c.strip_suffix('s') {
            (n, 1.0)
        } else if let Some(n) = c.strip_suffix('m') {
            (n, 60.0)
        } else if let Some(n) = c.strip_suffix('h') {
            (n, 3600.0)
        } else if let Some(n) = c.strip_suffix('d') {
            (n, 86_400.0)
        } else {
            continue;
        };
        if let Ok(v) = num.parse::<f64>() {
            if v > 0.0 {
                return Some(v * mult);
            }
        }
    }
    None
}

/// Plugin name for display: `"cpu-usage.10s.sh"` → `"cpu-usage"`.
pub fn display_name(name: &str) -> String {
    let base = name.split('.').next().unwrap_or(name);
    base.replace(['_', '-'], " ").trim().to_string()
}

/// Expand a leading `~`.
pub fn expand_tilde(p: &str) -> PathBuf {
    let p = p.trim();
    if let Some(rest) = p.strip_prefix("~/").or_else(|| (p == "~").then_some("")) {
        if let Some(h) = std::env::var_os("HOME") {
            return Path::new(&h).join(rest);
        }
    }
    PathBuf::from(p)
}

/// Plugins in `dir`: executable regular files (no dot files, no `.md` /
/// `.txt` docs), sorted by name.
pub fn list_plugins(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.starts_with('.') || [".md", ".txt", ".json", ".png", ".icns"].iter().any(|s| name.to_ascii_lowercase().ends_with(s)) {
                return false;
            }
            is_executable(p)
        })
        .collect();
    v.sort();
    v
}

#[cfg(unix)]
pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub fn is_executable(p: &Path) -> bool {
    p.is_file()
}

// ----- output format ----------------------------------------------------------

/// The parameters after `|` on a line.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Params {
    pub href: Option<String>,
    /// `bash=` / `shell=`.
    pub bash: Option<String>,
    /// `param1=`, `param2=`, … (also `param0`), in order.
    pub params: Vec<String>,
    /// `terminal=` (xbar runs in Terminal unless `terminal=false`).
    pub terminal: Option<bool>,
    pub refresh: bool,
    pub color: Option<String>,
    pub sfcolor: Option<String>,
    pub font: Option<String>,
    pub size: Option<f64>,
    pub length: Option<usize>,
    pub trim: bool,
    pub alternate: bool,
    pub dropdown: bool,
    pub disabled: bool,
    pub checked: bool,
    pub emojize: bool,
    pub ansi: bool,
    pub image: Option<String>,
    pub template_image: Option<String>,
    pub sfimage: Option<String>,
    pub tooltip: Option<String>,
    pub shortcut: Option<String>,
    /// Anything else, as given.
    pub other: Vec<(String, String)>,
}

/// One parsed line.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Line {
    pub text: String,
    pub params: Params,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Entry {
    Item { depth: u8, line: Line },
    Separator { depth: u8 },
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Output {
    /// Lines before the first `---` (the menu bar title; several cycle).
    pub titles: Vec<Line>,
    pub entries: Vec<Entry>,
}

/// Split `k=v k2="v 2" k3='v3'` into pairs (values may be quoted; a bare
/// value runs to the next space).
pub fn split_params(s: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        let ks = i;
        while i < c.len() && c[i] != '=' && !c[i].is_whitespace() {
            i += 1;
        }
        let key: String = c[ks..i].iter().collect();
        if i >= c.len() || c[i] != '=' {
            if !key.is_empty() {
                out.push((key, String::new()));
            }
            continue;
        }
        i += 1; // '='
        let mut val = String::new();
        if i < c.len() && (c[i] == '"' || c[i] == '\'') {
            let q = c[i];
            i += 1;
            while i < c.len() && c[i] != q {
                if c[i] == '\\' && i + 1 < c.len() && (c[i + 1] == q || c[i + 1] == '\\') {
                    i += 1;
                }
                val.push(c[i]);
                i += 1;
            }
            i += 1; // closing quote
        } else {
            while i < c.len() && !c[i].is_whitespace() {
                val.push(c[i]);
                i += 1;
            }
        }
        out.push((key, val));
    }
    out
}

fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1" | "")
}

pub fn parse_params(s: &str) -> Params {
    let mut p = Params { trim: true, dropdown: true, ..Default::default() };
    let mut numbered: Vec<(u32, String)> = vec![];
    for (k, v) in split_params(s) {
        let lk = k.to_ascii_lowercase();
        match lk.as_str() {
            "href" => p.href = Some(v),
            "bash" | "shell" => p.bash = Some(v),
            "terminal" => p.terminal = Some(truthy(&v)),
            "refresh" => p.refresh = truthy(&v),
            "color" => p.color = Some(v),
            "sfcolor" => p.sfcolor = Some(v),
            "font" => p.font = Some(v),
            "size" => p.size = v.parse().ok(),
            "length" => p.length = v.parse().ok(),
            "trim" => p.trim = truthy(&v),
            "alternate" => p.alternate = truthy(&v),
            "dropdown" => p.dropdown = truthy(&v),
            "disabled" => p.disabled = truthy(&v),
            "checked" => p.checked = truthy(&v),
            "emojize" => p.emojize = truthy(&v),
            "ansi" => p.ansi = truthy(&v),
            "image" => p.image = Some(v),
            "templateimage" => p.template_image = Some(v),
            "sfimage" => p.sfimage = Some(v),
            "tooltip" => p.tooltip = Some(v),
            "key" | "shortcut" => p.shortcut = Some(v),
            _ => match lk.strip_prefix("param").and_then(|n| n.parse::<u32>().ok()) {
                Some(n) => numbered.push((n, v)),
                None => p.other.push((k, v)),
            },
        }
    }
    numbered.sort_by_key(|(n, _)| *n);
    p.params = numbered.into_iter().map(|(_, v)| v).collect();
    p
}

/// Split a line at the first ` | ` (xbar also accepts `|` without spaces
/// when followed by `key=`).
fn split_line(raw: &str) -> (String, Params) {
    // The separator is the last "|" followed by something that parses
    // as key=value pairs, so a "|" inside the text survives.
    let mut idx = None;
    for (i, ch) in raw.char_indices().rev() {
        if ch == '|' {
            let rest = raw[i + 1..].trim();
            if rest.is_empty() || rest.split_whitespace().next().is_some_and(|w| w.contains('=')) {
                idx = Some(i);
                break;
            }
        }
    }
    match idx {
        Some(i) => (raw[..i].to_string(), parse_params(&raw[i + 1..])),
        None => (raw.to_string(), Params { trim: true, dropdown: true, ..Default::default() }),
    }
}

fn finish_text(text: &str, p: &Params) -> String {
    let mut t = text.to_string();
    // ANSI codes are removed whether or not ansi=true (we don't color runs).
    t = strip_ansi(&t);
    if p.trim {
        t = t.trim().to_string();
    }
    if p.emojize {
        t = emojize(&t);
    }
    if let Some(n) = p.length {
        if t.chars().count() > n {
            t = t.chars().take(n).collect::<String>() + "…";
        }
    }
    t
}

/// A few common GitHub-style emoji codes (`:white_check_mark:`); unknown
/// codes stay as they are.
pub fn emojize(s: &str) -> String {
    const MAP: &[(&str, &str)] = &[
        ("white_check_mark", "✅"),
        ("heavy_check_mark", "✔️"),
        ("x", "❌"),
        ("warning", "⚠️"),
        ("rocket", "🚀"),
        ("fire", "🔥"),
        ("star", "⭐"),
        ("sunny", "☀️"),
        ("cloud", "☁️"),
        ("umbrella", "☔"),
        ("snowflake", "❄️"),
        ("zap", "⚡"),
        ("bell", "🔔"),
        ("bug", "🐛"),
        ("hourglass", "⌛"),
        ("red_circle", "🔴"),
        ("large_blue_circle", "🔵"),
        ("green_heart", "💚"),
        ("heart", "❤️"),
        ("coffee", "☕"),
        ("computer", "💻"),
        ("moneybag", "💰"),
        ("chart_with_upwards_trend", "📈"),
        ("chart_with_downwards_trend", "📉"),
        ("+1", "👍"),
        ("thumbsup", "👍"),
        ("thumbsdown", "👎"),
        ("tada", "🎉"),
        ("lock", "🔒"),
        ("calendar", "📅"),
        ("email", "📧"),
        ("mailbox", "📫"),
    ];
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find(':') {
        let after = &rest[a + 1..];
        match after.find(':') {
            Some(b) if b > 0 && after[..b].chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '+' || c == '-') => {
                let code = &after[..b];
                match MAP.iter().find(|(k, _)| *k == code) {
                    Some((_, e)) => {
                        out.push_str(&rest[..a]);
                        out.push_str(e);
                        rest = &after[b + 1..];
                    }
                    None => {
                        out.push_str(&rest[..a + 1]);
                        rest = after;
                    }
                }
            }
            _ => {
                out.push_str(&rest[..a + 1]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse plugin output.
pub fn parse(out: &str) -> Output {
    let text = out.replace("\r\n", "\n");
    let mut o = Output::default();
    let mut in_body = false;
    for raw in text.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        if !in_body {
            if raw.trim() == "---" {
                in_body = true;
                continue;
            }
            let (t, p) = split_line(raw);
            o.titles.push(Line { text: finish_text(&t, &p), params: p });
            continue;
        }
        // Depth: leading "--" pairs.
        let mut depth = 0u8;
        let mut s = raw;
        loop {
            let t = s.trim_start();
            if t.trim_end() == "---" {
                o.entries.push(Entry::Separator { depth });
                break;
            }
            if let Some(r) = s.strip_prefix("--") {
                depth = depth.saturating_add(1);
                s = r;
                continue;
            }
            let (t, p) = split_line(s);
            o.entries.push(Entry::Item { depth, line: Line { text: finish_text(&t, &p), params: p } });
            break;
        }
    }
    o
}

/// Minimal base64 (standard / URL-safe alphabets, padding optional,
/// whitespace ignored).
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn image_of(p: &Params) -> Option<ImageData> {
    let (b64, template) = match (&p.template_image, &p.image) {
        (Some(t), _) => (t, true),
        (None, Some(i)) => (i, false),
        _ => return None,
    };
    base64_decode(b64).filter(|b| b.len() > 8).map(|b| ImageData::new(b, template))
}

/// The action of a line: `href` opens a URL; `bash` runs the program with
/// its params (in Terminal unless `terminal=false`); `refresh=true` alone
/// just refreshes.
pub fn action_of(p: &Params) -> Option<ItemAction> {
    if let Some(cmd) = p.bash.as_ref().filter(|b| !b.trim().is_empty()) {
        return Some(ItemAction::Run { program: cmd.clone(), args: p.params.clone(), shell: false, terminal: p.terminal.unwrap_or(true), refresh: p.refresh });
    }
    if let Some(h) = p.href.as_ref().filter(|h| !h.trim().is_empty()) {
        return Some(ItemAction::OpenUrl(h.trim().to_string()));
    }
    p.refresh.then_some(ItemAction::Refresh)
}

fn is_mono_font(f: &str) -> bool {
    let f = f.to_ascii_lowercase();
    ["mono", "menlo", "courier", "consol", "code"].iter().any(|k| f.contains(k))
}

fn item_of(depth: u8, l: &Line) -> Item {
    let p = &l.params;
    Item {
        title: l.text.clone(),
        subtitle: String::new(),
        action: action_of(p),
        color: p.color.as_deref().and_then(parse_color),
        symbol: p.sfimage.clone().filter(|s| !s.is_empty()),
        image: image_of(p),
        depth,
        separator: false,
        header: false,
        checked: p.checked,
        disabled: p.disabled,
        mono: p.font.as_deref().is_some_and(is_mono_font),
        refresh: p.refresh,
    }
}

/// Plugin output → card content: the first visible title line is the
/// title (the others cycle), the dropdown entries are the items
/// (alternates and `dropdown=false` lines left out, separators kept).
pub fn to_content(o: &Output) -> Content {
    let titles: Vec<&Line> = o.titles.iter().filter(|l| !l.params.alternate).collect();
    let first = titles.first();
    let mut c = Content { style: Style::List, ..Default::default() };
    if let Some(l) = first {
        c.title = l.text.clone();
        c.color = l.params.color.as_deref().and_then(parse_color);
        c.symbol = l.params.sfimage.clone().filter(|s| !s.is_empty());
        c.image = image_of(&l.params);
        c.action = action_of(&l.params);
    }
    c.alt_titles = titles.iter().skip(1).map(|l| l.text.clone()).collect();
    // Title lines with dropdown=true (the default) also show at the top
    // of the menu in xbar when there are several; we keep them as titles.
    for e in &o.entries {
        match e {
            Entry::Separator { depth } => {
                // No leading / doubled separators.
                if c.items.last().is_some_and(|i| !i.separator) {
                    c.items.push(Item { separator: true, depth: *depth, ..Default::default() });
                }
            }
            Entry::Item { depth, line } => {
                if line.params.alternate || !line.params.dropdown {
                    continue;
                }
                c.items.push(item_of(*depth, line));
            }
        }
    }
    while c.items.last().is_some_and(|i| i.separator) {
        c.items.pop();
    }
    if c.items.is_empty() {
        c.style = Style::Text;
    }
    c
}

/// Folder mode: one section per plugin (a header row with the plugin's
/// title, then its top-level items).
pub fn merge_sections(sections: &[(String, Result<Content, String>)]) -> Content {
    let mut c = Content { style: Style::List, ..Default::default() };
    for (name, r) in sections {
        match r {
            Ok(s) => {
                let title = if s.title.is_empty() { name.clone() } else { s.title.clone() };
                c.items.push(Item { title, subtitle: name.clone(), header: true, color: s.color, symbol: s.symbol.clone(), image: s.image.clone(), action: s.action.clone(), ..Default::default() });
                c.items.extend(s.items.iter().filter(|i| i.depth == 0 && !i.separator).take(6).cloned());
            }
            Err(e) => c.items.push(Item { title: name.clone(), subtitle: e.clone(), header: true, color: parse_color("orange"), symbol: Some("exclamationmark.triangle.fill".into()), ..Default::default() }),
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_from_file_names() {
        assert_eq!(interval_from_name("cpu.10s.sh"), Some(10.0));
        assert_eq!(interval_from_name("weather.5m.py"), Some(300.0));
        assert_eq!(interval_from_name("github-notifications.1h.rb"), Some(3600.0));
        assert_eq!(interval_from_name("backup.1d.js"), Some(86_400.0));
        assert_eq!(interval_from_name("fast.500ms.sh"), Some(0.5));
        assert_eq!(interval_from_name("name.30s"), Some(30.0));
        assert_eq!(interval_from_name("plain.sh"), None);
        assert_eq!(interval_from_name("my.plugin.sh"), None);
        assert_eq!(interval_from_name("zero.0s.sh"), None);
        assert_eq!(interval_from_name("noext"), None);
        assert_eq!(display_name("cpu-usage.10s.sh"), "cpu usage");
    }

    #[test]
    fn param_splitting() {
        let p = split_params(r#"bash="/usr/local/bin/my script" param1=--flag param2='two words' terminal=false color=#ff0000 refresh"#);
        assert_eq!(p[0], ("bash".into(), "/usr/local/bin/my script".into()));
        assert_eq!(p[2], ("param2".into(), "two words".into()));
        assert_eq!(p[5], ("refresh".into(), "".into()));
        let q = parse_params(r#"param2=b param10=j param1=a PARAM3="c \"q\"" unknown=1"#);
        assert_eq!(q.params, vec!["a", "b", "c \"q\"", "j"]);
        assert_eq!(q.other, vec![("unknown".to_string(), "1".to_string())]);
        assert!(q.trim && q.dropdown);
    }

    // The canonical xbar example plugin output.
    const SAMPLE: &str = "\
☀️ 21°C | color=orange
:umbrella: Rain later | emojize=true dropdown=false
---
Berlin, Germany | size=14 font=HelveticaNeue-Bold
Open forecast | href=https://open-meteo.com color=#0a84ff sfimage=safari
---
Refresh | refresh=true
Run backup | bash=/usr/local/bin/backup.sh param1=--quick terminal=false
Settings
--Units: Celsius | checked=true
--Units: Fahrenheit | bash='/bin/echo' param1=\"set f\" terminal=false refresh=true
-----
--Advanced
----Debug log | href=file:///tmp/x.log
Hidden alt | alternate=true
";

    #[test]
    fn parses_a_typical_plugin() {
        let o = parse(SAMPLE);
        assert_eq!(o.titles.len(), 2);
        assert_eq!(o.titles[0].text, "☀️ 21°C");
        assert_eq!(o.titles[0].params.color.as_deref(), Some("orange"));
        assert_eq!(o.titles[1].text, "☔ Rain later");
        assert!(!o.titles[1].params.dropdown);
        let depths: Vec<(u8, bool)> = o.entries.iter().map(|e| match e {
            Entry::Item { depth, .. } => (*depth, false),
            Entry::Separator { depth } => (*depth, true),
        }).collect();
        assert_eq!(depths, vec![(0, false), (0, false), (0, true), (0, false), (0, false), (0, false), (1, false), (1, false), (1, true), (1, false), (2, false), (0, false)]);
        let c = to_content(&o);
        assert_eq!(c.title, "☀️ 21°C");
        assert_eq!(c.alt_titles, vec!["☔ Rain later"]);
        assert!(c.color.is_some());
        let titles: Vec<&str> = c.items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, vec!["Berlin, Germany", "Open forecast", "", "Refresh", "Run backup", "Settings", "Units: Celsius", "Units: Fahrenheit", "", "Advanced", "Debug log"]);
        assert_eq!(c.items[1].action, Some(ItemAction::OpenUrl("https://open-meteo.com".into())));
        assert_eq!(c.items[1].symbol.as_deref(), Some("safari"));
        assert_eq!(c.items[3].action, Some(ItemAction::Refresh));
        assert_eq!(c.items[4].action, Some(ItemAction::Run { program: "/usr/local/bin/backup.sh".into(), args: vec!["--quick".into()], shell: false, terminal: false, refresh: false }));
        assert!(c.items[6].checked && c.items[6].depth == 1);
        assert_eq!(c.items[7].action, Some(ItemAction::Run { program: "/bin/echo".into(), args: vec!["set f".into()], shell: false, terminal: false, refresh: true }));
        assert_eq!(c.items[10].depth, 2);
    }

    #[test]
    fn title_only_and_edge_cases() {
        let c = to_content(&parse("42%\n"));
        assert_eq!((c.title.as_str(), c.style, c.items.len()), ("42%", Style::Text, 0));
        // Pipes inside the text that aren't parameters.
        let o = parse("a | b\n---\nx|y | color=red\nfoo|bar");
        assert_eq!(o.titles[0].text, "a | b");
        let Entry::Item { line, .. } = &o.entries[0] else { panic!() };
        assert_eq!((line.text.as_str(), line.params.color.as_deref()), ("x|y", Some("red")));
        let Entry::Item { line, .. } = &o.entries[1] else { panic!() };
        assert_eq!(line.text, "foo|bar");
        // Leading / trailing / doubled separators are dropped.
        let c = to_content(&parse("T\n---\n---\nA\n---\n---\nB\n---\n"));
        let t: Vec<bool> = c.items.iter().map(|i| i.separator).collect();
        assert_eq!(t, vec![false, true, false]);
        // CRLF, ANSI, length, trim=false.
        let c = to_content(&parse("\u{1b}[31mRed\u{1b}[0m | ansi=true\r\n---\r\nA very long item text | length=6\r\n  spaced  | trim=false\r\n"));
        assert_eq!(c.title, "Red");
        assert_eq!(c.items[0].title, "A very…");
        assert_eq!(c.items[1].title, "  spaced  ");
        // Empty output.
        assert!(to_content(&parse("")).is_empty());
        // xbar's default: bash without terminal= runs in Terminal.
        let c = to_content(&parse("t\n---\nx | bash=/bin/ls"));
        assert!(matches!(&c.items[0].action, Some(ItemAction::Run { terminal: true, .. })));
        // shell= is a synonym (SwiftBar), href + refresh
        let c = to_content(&parse("t\n---\nx | shell=/bin/ls param0=-la\ny | href=https://a.b refresh=true"));
        assert!(matches!(&c.items[0].action, Some(ItemAction::Run { args, .. }) if args == &vec!["-la".to_string()]));
        assert_eq!(c.items[1].action, Some(ItemAction::OpenUrl("https://a.b".into())));
        assert!(c.items[1].refresh);
        // Fonts.
        let c = to_content(&parse("t\n---\nlog | font=Menlo size=11\nx | font=Helvetica"));
        assert!(c.items[0].mono && !c.items[1].mono);
        // Disabled / checked.
        let c = to_content(&parse("t\n---\noff | disabled=true\non | checked=true"));
        assert!(c.items[0].disabled && c.items[1].checked);
    }

    // Shapes of popular community plugins.
    #[test]
    fn real_world_samples() {
        // GitHub notifications style.
        let gh = "#[fg=white]🔔 3\n---\nNotifications | href=https://github.com/notifications\n---\nrust-lang/rust\n--Fix ICE in borrowck | href=https://github.com/rust-lang/rust/pull/1 color=#6e7681\n--Tracking issue | href=https://github.com/rust-lang/rust/issues/2\n";
        let c = to_content(&parse(gh));
        assert_eq!(c.items.iter().filter(|i| i.depth == 1).count(), 2);
        // Bitcoin ticker with a templateImage.
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let btc = format!("$67,212 | templateImage={png_b64}\n---\nCoinbase | href=https://coinbase.com\n");
        let c = to_content(&parse(&btc));
        let img = c.image.unwrap();
        assert!(img.template);
        assert_eq!(&img.bytes[1..4], b"PNG");
        // SwiftBar sfimage + sfcolor + tooltip, emoji title.
        let sb = "📦 2 | sfimage=shippingbox.fill sfcolor=#ffcc00\n---\nUPS: Out for delivery | tooltip=\"ETA 5 pm\" color=green\nDHL: In transit | color=yellow";
        let o = parse(sb);
        assert_eq!(o.titles[0].params.sfcolor.as_deref(), Some("#ffcc00"));
        let c = to_content(&o);
        assert_eq!(c.symbol.as_deref(), Some("shippingbox.fill"));
        assert_eq!(c.items.len(), 2);
        assert!(c.items.iter().all(|i| i.color.is_some()));
        // Cycling titles, nothing else.
        let c = to_content(&parse("CPU 12%\nMEM 61%\nNET 2 MB/s\n"));
        assert_eq!(c.alt_titles.len(), 2);
        // Folder mode merge.
        let m = merge_sections(&[("cpu".into(), Ok(to_content(&parse("CPU 12%\n---\nTop | bash=top")))), ("bad".into(), Err("exit 1".into()))]);
        assert_eq!(m.items.len(), 3);
        assert!(m.items[0].header && m.items[2].header);
        assert_eq!(m.items[2].subtitle, "exit 1");
    }

    #[test]
    fn base64_and_emoji() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVs\nbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("_-8").unwrap(), vec![0xff, 0xef]);
        assert!(base64_decode("a$b").is_none());
        assert_eq!(emojize(":rocket: launch :nope: 10:30 :x:"), "🚀 launch :nope: 10:30 ❌");
    }

    #[test]
    #[cfg(unix)]
    fn plugin_folder_listing() {
        let dir = std::env::temp_dir().join(format!("ns-xbar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (n, exec) in [("b.5m.sh", true), ("a.10s.py", true), ("README.md", true), (".hidden.sh", true), ("data.1m.sh", false)] {
            let p = dir.join(n);
            std::fs::write(&p, "#!/bin/sh\necho hi\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
            }
        }
        let names: Vec<String> = list_plugins(&dir).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        #[cfg(unix)]
        assert_eq!(names, vec!["a.10s.py", "b.5m.sh"]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(expand_tilde("~/x").ends_with("x") && !expand_tilde("~/x").starts_with("~"));
        assert_eq!(expand_tilde("/a/b"), PathBuf::from("/a/b"));
    }
}
