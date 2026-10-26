//! The content model shared by the "build your own" widgets (Script, xbar
//! plugins, Web / JSON, Shortcuts) and the parser for script output in its
//! four formats: plain text, a number with a unit, a list and JSON.

use std::sync::Arc;

use serde_json::Value;

use super::catalog::OutputFormat;
use super::Rgba;

/// An image delivered by a plugin (base64 PNG / JPEG), decoded by the UI.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageData {
    pub bytes: Arc<Vec<u8>>,
    /// Template images are tinted to match the text.
    pub template: bool,
    /// Content hash (cache key).
    pub key: u64,
}

impl ImageData {
    pub fn new(bytes: Vec<u8>, template: bool) -> Self {
        let key = hash_bytes(&bytes) ^ (template as u64);
        ImageData { bytes: Arc::new(bytes), template, key }
    }
}

pub fn hash_bytes(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for x in (b.len() as u64).to_le_bytes().iter().chain(b.iter()) {
        h ^= *x as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// What clicking an item (or the card) does.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemAction {
    OpenUrl(String),
    /// Run a program (`program` + `args`), or a shell command when `args`
    /// is empty and `shell` is set.
    Run { program: String, args: Vec<String>, shell: bool, terminal: bool, refresh: bool },
    Copy(String),
    /// Re-run the widget's command / plugin / request.
    Refresh,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Item {
    pub title: String,
    pub subtitle: String,
    pub action: Option<ItemAction>,
    pub color: Option<Rgba>,
    pub symbol: Option<String>,
    pub image: Option<ImageData>,
    /// Submenu level (0 = top).
    pub depth: u8,
    pub separator: bool,
    /// A section header (plugin name in folder mode).
    pub header: bool,
    pub checked: bool,
    pub disabled: bool,
    /// Monospaced font (xbar `font=Menlo` etc.).
    pub mono: bool,
    /// After running the action, refresh the widget.
    pub refresh: bool,
}

impl Item {
    pub fn text(title: &str) -> Item {
        Item { title: title.to_string(), ..Default::default() }
    }
}

/// How the card lays the content out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Style {
    /// Title + body text.
    #[default]
    Text,
    /// A big value (+ unit), optional gauge.
    Number,
    /// Title + item rows.
    List,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Content {
    pub style: Style,
    pub title: String,
    pub subtitle: String,
    pub body: String,
    /// The value as text ("42.5"), for the Number style.
    pub value: Option<String>,
    pub number: Option<f64>,
    pub unit: String,
    /// 0..1, when the source gave one (JSON `progress`, a percentage).
    pub progress: Option<f64>,
    pub color: Option<Rgba>,
    pub symbol: Option<String>,
    pub image: Option<ImageData>,
    pub items: Vec<Item>,
    /// Extra title lines to cycle through (xbar).
    pub alt_titles: Vec<String>,
    /// Card click action (from the content).
    pub action: Option<ItemAction>,
}

impl Content {
    pub fn is_empty(&self) -> bool {
        self.title.trim().is_empty() && self.body.trim().is_empty() && self.value.is_none() && self.items.is_empty()
    }

    /// Gauge fraction: the content's own progress, else `number` scaled
    /// from [min, max] (percentages from [0, 100]).
    pub fn gauge(&self, min: f64, max: f64) -> Option<f64> {
        if let Some(p) = self.progress {
            return Some(p.clamp(0.0, 1.0));
        }
        let n = self.number?;
        let (lo, hi) = if self.unit == "%" { (0.0, 100.0) } else { (min, max) };
        (hi > lo).then(|| ((n - lo) / (hi - lo)).clamp(0.0, 1.0))
    }
}

/// Loading / ready / failed, with the last good value kept on failure.
#[derive(Clone, Debug, PartialEq, Default)]
#[allow(clippy::large_enum_variant)]
pub enum Status {
    #[default]
    Loading,
    Ready { content: Content, at: f64 },
    Error { message: String, stale: Option<Box<Content>>, at: f64 },
}

impl Status {
    pub fn content(&self) -> Option<&Content> {
        match self {
            Status::Ready { content, .. } => Some(content),
            Status::Error { stale, .. } => stale.as_deref(),
            Status::Loading => None,
        }
    }

    /// The next status after a run: errors keep the last good content.
    pub fn next(self, r: Result<Content, String>, at: f64) -> Status {
        match r {
            Ok(content) => Status::Ready { content, at },
            Err(message) => {
                let stale = match self {
                    Status::Ready { content, .. } => Some(Box::new(content)),
                    Status::Error { stale, .. } => stale,
                    Status::Loading => None,
                };
                Status::Error { message, stale, at }
            }
        }
    }
}

// ----- colors ---------------------------------------------------------------

/// "#rgb", "#rrggbb", "#rrggbbaa", a CSS-ish name, or a "light,dark" pair
/// (the dark one is used: the notch is black).
pub fn parse_color(s: &str) -> Option<Rgba> {
    let s = s.trim();
    if let Some((_, dark)) = s.split_once(',') {
        return parse_color(dark);
    }
    if let Some(h) = s.strip_prefix('#') {
        let hex = |i: usize, n: usize| u8::from_str_radix(h.get(i..i + n)?, 16).ok();
        return match h.len() {
            3 => {
                let c = |i| hex(i, 1).map(|v| v as f64 * 17.0 / 255.0);
                Some(Rgba(c(0)?, c(1)?, c(2)?, 1.0))
            }
            6 | 8 => {
                let c = |i| hex(i, 2).map(|v| v as f64 / 255.0);
                Some(Rgba(c(0)?, c(2)?, c(4)?, if h.len() == 8 { c(6)? } else { 1.0 }))
            }
            _ => None,
        };
    }
    let rgb = |r: u8, g: u8, b: u8| Some(Rgba(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, 1.0));
    // Apple's dark-mode system colors.
    match s.to_ascii_lowercase().as_str() {
        "red" => rgb(255, 69, 58),
        "orange" => rgb(255, 159, 10),
        "yellow" => rgb(255, 214, 10),
        "green" => rgb(48, 209, 88),
        "mint" => rgb(99, 230, 226),
        "teal" => rgb(64, 200, 224),
        "cyan" => rgb(100, 210, 255),
        "blue" => rgb(10, 132, 255),
        "indigo" => rgb(94, 92, 230),
        "purple" | "violet" => rgb(191, 90, 242),
        "pink" | "magenta" => rgb(255, 55, 95),
        "brown" => rgb(172, 142, 104),
        "gray" | "grey" => rgb(152, 152, 157),
        "white" => rgb(255, 255, 255),
        "black" => rgb(70, 70, 74), // invisible on black otherwise
        "lightgray" | "lightgrey" | "silver" => rgb(199, 199, 204),
        "darkgray" | "darkgrey" => rgb(99, 99, 102),
        "gold" => rgb(255, 204, 0),
        _ => None,
    }
}

/// Remove ANSI escape sequences (colors, cursor moves).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            match it.peek() {
                Some('[') => {
                    it.next();
                    for d in it.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: up to BEL or ESC \.
                    it.next();
                    while let Some(d) = it.next() {
                        if d == '\u{7}' || (d == '\u{1b}' && it.peek() == Some(&'\\')) {
                            if d == '\u{1b}' {
                                it.next();
                            }
                            break;
                        }
                    }
                }
                _ => {
                    it.next();
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ----- numbers --------------------------------------------------------------

/// A number found in a line: `"CPU: 42.5 %"` → label "CPU", 42.5, "%".
#[derive(Clone, Debug, PartialEq)]
pub struct Num {
    pub label: String,
    pub value: f64,
    pub text: String,
    pub unit: String,
}

/// Find the first number in `line` (sign, thousands separators, decimals,
/// exponent). The text before it is a label, the text after a unit.
pub fn parse_number(line: &str) -> Option<Num> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let sign = (c == b'-' || c == b'+') && b.get(i + 1).is_some_and(|d| d.is_ascii_digit() || *d == b'.');
        if c.is_ascii_digit() || sign || (c == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            // A digit inside a word ("utf8") is not a number.
            if i > 0 && (b[i - 1].is_ascii_alphabetic() || b[i - 1] == b'_') && !sign {
                i += 1;
                continue;
            }
            let start = i;
            let mut j = i;
            if sign {
                j += 1;
            }
            let mut txt = String::new();
            if b[start] == b'-' {
                txt.push('-');
            }
            let mut seen_dot = false;
            let mut seen_e = false;
            while j < b.len() {
                let d = b[j];
                if d.is_ascii_digit() {
                    txt.push(d as char);
                } else if d == b'.' && !seen_dot && !seen_e && b.get(j + 1).is_some_and(|x| x.is_ascii_digit()) {
                    seen_dot = true;
                    txt.push('.');
                } else if d == b',' && !seen_dot && b.get(j + 1..j + 4).is_some_and(|x| x.iter().all(|y| y.is_ascii_digit())) && !b.get(j + 4).is_some_and(|x| x.is_ascii_digit()) {
                    // thousands separator
                } else if (d == b'e' || d == b'E') && !seen_e && b.get(j + 1).is_some_and(|x| x.is_ascii_digit() || ((*x == b'-' || *x == b'+') && b.get(j + 2).is_some_and(|y| y.is_ascii_digit()))) {
                    seen_e = true;
                    txt.push('e');
                    if b[j + 1] == b'-' || b[j + 1] == b'+' {
                        j += 1;
                        txt.push(b[j] as char);
                    }
                } else {
                    break;
                }
                j += 1;
            }
            let value: f64 = txt.parse().ok()?;
            let label = line[..start].trim().trim_end_matches([':', '=', '-']).trim().to_string();
            let unit = line[j..].trim().to_string();
            return Some(Num { label, value, text: line[start..j].trim_start_matches('+').to_string(), unit });
        }
        i += 1;
    }
    None
}

// ----- script output ----------------------------------------------------------

fn clean(out: &str) -> String {
    strip_ansi(out).replace("\r\n", "\n").replace('\r', "\n")
}

/// Parse script output in `fmt`. `Auto` picks JSON for an object / array,
/// Number for a single line with a number, else Text.
pub fn parse_output(out: &str, fmt: OutputFormat) -> Result<Content, String> {
    let s = clean(out);
    let t = s.trim();
    match fmt {
        OutputFormat::Auto => {
            if t.starts_with('{') || t.starts_with('[') {
                if let Ok(c) = parse_json(t) {
                    return Ok(c);
                }
            }
            let lines: Vec<&str> = t.lines().filter(|l| !l.trim().is_empty()).collect();
            if lines.len() <= 2 && !lines.is_empty() {
                if let Ok(c) = parse_number_output(t) {
                    // "42%" / "CPU 42%" with at most a caption line; a
                    // sentence that happens to contain a digit stays text.
                    let n = parse_number(lines[0]).unwrap();
                    if n.label.chars().count() <= 24 && n.unit.chars().count() <= 12 {
                        return Ok(c);
                    }
                }
            }
            Ok(parse_text(t))
        }
        OutputFormat::Text => Ok(parse_text(t)),
        OutputFormat::Number => parse_number_output(t),
        OutputFormat::List => Ok(parse_list(t)),
        OutputFormat::Json => parse_json(t),
    }
}

/// First line = title, the rest = body.
pub fn parse_text(t: &str) -> Content {
    let mut lines = t.lines();
    let title = lines.next().unwrap_or("").trim().to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Content { style: Style::Text, title, body, ..Default::default() }
}

/// A number (with label and unit) on the first line, a caption on the
/// second.
pub fn parse_number_output(t: &str) -> Result<Content, String> {
    let mut lines = t.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().ok_or("No output")?;
    let n = parse_number(first).ok_or_else(|| format!("No number in \u{201c}{}\u{201d}", first.trim()))?;
    let subtitle = lines.next().unwrap_or("").trim().to_string();
    let progress = (n.unit == "%").then(|| (n.value / 100.0).clamp(0.0, 1.0));
    Ok(Content { style: Style::Number, title: n.label, subtitle, value: Some(n.text), number: Some(n.value), unit: n.unit, progress, ..Default::default() })
}

/// One item per line. `title<TAB>subtitle`; `- ` / `* ` bullets and
/// `[x]` / `[ ]` check boxes are understood.
pub fn parse_list(t: &str) -> Content {
    let items = t
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut l = l.trim();
            for b in ["- ", "* ", "• "] {
                if let Some(r) = l.strip_prefix(b) {
                    l = r;
                }
            }
            let mut it = Item::default();
            if let Some(r) = l.strip_prefix("[x] ").or_else(|| l.strip_prefix("[X] ")) {
                it.checked = true;
                l = r;
            } else if let Some(r) = l.strip_prefix("[ ] ") {
                l = r;
            }
            let (title, sub) = l.split_once('\t').unwrap_or((l, ""));
            it.title = title.trim().to_string();
            it.subtitle = sub.trim().to_string();
            it
        })
        .collect();
    Content { style: Style::List, items, ..Default::default() }
}

/// An action from JSON: "https://…", "copy:TEXT", "run:COMMAND",
/// "refresh", or `{"type": "url"|"run"|"copy"|"refresh", "value": …}`.
pub fn parse_action(v: &Value) -> Option<ItemAction> {
    match v {
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            if let Some(r) = s.strip_prefix("copy:") {
                return Some(ItemAction::Copy(r.to_string()));
            }
            if let Some(r) = s.strip_prefix("run:").or_else(|| s.strip_prefix("shell:")) {
                return Some(ItemAction::Run { program: r.trim().to_string(), args: vec![], shell: true, terminal: false, refresh: false });
            }
            if s == "refresh" {
                return Some(ItemAction::Refresh);
            }
            s.contains(':').then(|| ItemAction::OpenUrl(s.to_string()))
        }
        Value::Object(o) => {
            let val = o.get("value").or_else(|| o.get("url")).or_else(|| o.get("command")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let refresh = o.get("refresh").and_then(|v| v.as_bool()).unwrap_or(false);
            match o.get("type").and_then(|t| t.as_str()).unwrap_or(if o.contains_key("url") { "url" } else { "run" }) {
                "url" | "open" | "href" => (!val.is_empty()).then_some(ItemAction::OpenUrl(val)),
                "copy" => Some(ItemAction::Copy(val)),
                "refresh" => Some(ItemAction::Refresh),
                "run" | "shell" | "bash" => (!val.is_empty()).then_some(ItemAction::Run {
                    program: val,
                    args: vec![],
                    shell: true,
                    terminal: o.get("terminal").and_then(|v| v.as_bool()).unwrap_or(false),
                    refresh,
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

fn str_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn symbol_of(o: &serde_json::Map<String, Value>) -> Option<String> {
    ["sf_symbol", "sfSymbol", "symbol", "sfimage"].iter().find_map(|k| o.get(*k).and_then(|v| v.as_str())).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn item_of(v: &Value) -> Option<Item> {
    match v {
        Value::String(s) => Some(Item::text(s)),
        Value::Number(_) | Value::Bool(_) => Some(Item::text(&str_of(Some(v)))),
        Value::Object(o) => Some(Item {
            title: str_of(o.get("title").or_else(|| o.get("text")).or_else(|| o.get("name"))),
            subtitle: str_of(o.get("subtitle").or_else(|| o.get("detail")).or_else(|| o.get("value"))),
            action: o.get("action").or_else(|| o.get("href")).and_then(parse_action),
            color: o.get("color").and_then(|c| c.as_str()).and_then(parse_color),
            symbol: symbol_of(o),
            checked: o.get("checked").and_then(|v| v.as_bool()).unwrap_or(false),
            separator: o.get("separator").and_then(|v| v.as_bool()).unwrap_or(false),
            ..Default::default()
        }),
        _ => None,
    }
}

/// `{title, subtitle, value, unit, progress, items: [{title, subtitle,
/// action, color, sf_symbol}], color, sf_symbol, action}` or a bare array
/// of items.
pub fn parse_json(t: &str) -> Result<Content, String> {
    let v: Value = serde_json::from_str(t).map_err(|e| format!("Invalid JSON: {e}"))?;
    let o = match &v {
        Value::Array(a) => return Ok(Content { style: Style::List, items: a.iter().filter_map(item_of).collect(), ..Default::default() }),
        Value::Object(o) => o,
        _ => return Err("Expected a JSON object".into()),
    };
    let mut c = Content {
        title: str_of(o.get("title")),
        subtitle: str_of(o.get("subtitle")),
        body: str_of(o.get("body").or_else(|| o.get("text"))),
        unit: str_of(o.get("unit")),
        color: o.get("color").and_then(|c| c.as_str()).and_then(parse_color),
        symbol: symbol_of(o),
        action: o.get("action").or_else(|| o.get("href")).and_then(parse_action),
        ..Default::default()
    };
    match o.get("value") {
        Some(Value::Number(n)) => {
            c.number = n.as_f64();
            c.value = Some(fmt_value(n.as_f64().unwrap_or(0.0)));
        }
        Some(Value::String(s)) if !s.is_empty() => {
            // "42%" → number 42 with unit "%".
            match parse_number(s).filter(|n| n.label.is_empty()) {
                Some(n) => {
                    c.number = Some(n.value);
                    c.value = Some(n.text);
                    if c.unit.is_empty() {
                        c.unit = n.unit;
                    }
                }
                None => c.value = Some(s.clone()),
            }
        }
        _ => {}
    }
    c.progress = o.get("progress").and_then(|p| p.as_f64()).map(|p| if p > 1.0 && p <= 100.0 { p / 100.0 } else { p.clamp(0.0, 1.0) });
    if c.progress.is_none() && c.unit == "%" {
        c.progress = c.number.map(|n| (n / 100.0).clamp(0.0, 1.0));
    }
    if let Some(Value::Array(items)) = o.get("items") {
        c.items = items.iter().filter_map(item_of).collect();
    }
    c.style = if c.value.is_some() {
        Style::Number
    } else if !c.items.is_empty() {
        Style::List
    } else {
        Style::Text
    };
    Ok(c)
}

/// 42 → "42", 3.14159 → "3.14", 1234567 → "1234567".
pub fn fmt_value(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else if n.abs() >= 100.0 {
        format!("{n:.1}")
    } else {
        let s = format!("{n:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_in_lines() {
        let n = parse_number("CPU: 42.5 %").unwrap();
        assert_eq!((n.label.as_str(), n.value, n.text.as_str(), n.unit.as_str()), ("CPU", 42.5, "42.5", "%"));
        let n = parse_number("1,234,567 requests").unwrap();
        assert_eq!((n.value, n.unit.as_str()), (1_234_567.0, "requests"));
        let n = parse_number("-3.2GB").unwrap();
        assert_eq!((n.value, n.unit.as_str(), n.text.as_str()), (-3.2, "GB", "-3.2"));
        assert_eq!(parse_number("72°F").unwrap().unit, "°F");
        assert_eq!(parse_number(".5").unwrap().value, 0.5);
        assert_eq!(parse_number("1.5e3 ms").unwrap().value, 1500.0);
        assert_eq!(parse_number("utf8 is 8 bits").unwrap().value, 8.0);
        assert_eq!(parse_number("Temp = +21").unwrap().label, "Temp");
        assert_eq!(parse_number("1,5").unwrap().value, 1.0, "a comma is not a decimal separator");
        assert!(parse_number("no digits").is_none());
    }

    #[test]
    fn text_output() {
        let c = parse_output("Build #42 passed\nmain · 3 min ago\n", OutputFormat::Text).unwrap();
        assert_eq!((c.style, c.title.as_str(), c.body.as_str()), (Style::Text, "Build #42 passed", "main · 3 min ago"));
        // Auto: a sentence with several lines stays text.
        let c = parse_output("Disk is fine\nSSD: 120 GB free\nNo errors", OutputFormat::Auto).unwrap();
        assert_eq!(c.style, Style::Text);
        let c = parse_output("", OutputFormat::Auto).unwrap();
        assert!(c.is_empty());
        let c = parse_output("\u{1b}[32mgreen\u{1b}[0m text\r\nnext", OutputFormat::Text).unwrap();
        assert_eq!((c.title.as_str(), c.body.as_str()), ("green text", "next"));
    }

    #[test]
    fn number_output() {
        let c = parse_output("73%\n", OutputFormat::Auto).unwrap();
        assert_eq!((c.style, c.value.as_deref(), c.unit.as_str(), c.progress), (Style::Number, Some("73"), "%", Some(0.73)));
        let c = parse_output("Load 2.41\nlast minute", OutputFormat::Auto).unwrap();
        assert_eq!((c.title.as_str(), c.number, c.subtitle.as_str(), c.progress), ("Load", Some(2.41), "last minute", None));
        assert_eq!(c.gauge(0.0, 8.0), Some(2.41 / 8.0));
        assert_eq!(c.gauge(5.0, 5.0), None);
        let c = parse_output("  12 GB  ", OutputFormat::Number).unwrap();
        assert_eq!((c.number, c.unit.as_str()), (Some(12.0), "GB"));
        assert!(parse_output("hello", OutputFormat::Number).unwrap_err().contains("No number"));
        assert!(parse_output("", OutputFormat::Number).is_err());
        // A long sentence with a number isn't a number widget in auto mode.
        let c = parse_output("The quick brown fox jumped over 3 lazy dogs today", OutputFormat::Auto).unwrap();
        assert_eq!(c.style, Style::Text);
    }

    #[test]
    fn list_output() {
        let c = parse_output("- [x] Write tests\n* [ ] Ship it\tFriday\n\nplain line\n", OutputFormat::List).unwrap();
        assert_eq!(c.style, Style::List);
        assert_eq!(c.items.len(), 3);
        assert!(c.items[0].checked && !c.items[1].checked);
        assert_eq!((c.items[1].title.as_str(), c.items[1].subtitle.as_str()), ("Ship it", "Friday"));
        assert_eq!(c.items[2].title, "plain line");
    }

    #[test]
    fn json_output() {
        let j = r##"{"title":"Deploys","subtitle":"prod","value":"97.5%","color":"#30d158","sf_symbol":"bolt.fill",
            "items":[{"title":"api","subtitle":"ok","action":"https://ci.example.com/api","color":"green"},
                     {"title":"web","action":{"type":"run","value":"open -a Safari","refresh":true}},
                     {"title":"copy me","action":"copy:secret-id"}, "plain", 5]}"##;
        let c = parse_output(j, OutputFormat::Auto).unwrap();
        assert_eq!(c.style, Style::Number);
        assert_eq!((c.number, c.unit.as_str(), c.progress), (Some(97.5), "%", Some(0.975)));
        assert_eq!(c.symbol.as_deref(), Some("bolt.fill"));
        assert_eq!(c.color, Some(Rgba(48.0 / 255.0, 209.0 / 255.0, 88.0 / 255.0, 1.0)));
        assert_eq!(c.items.len(), 5);
        assert_eq!(c.items[0].action, Some(ItemAction::OpenUrl("https://ci.example.com/api".into())));
        assert!(matches!(&c.items[1].action, Some(ItemAction::Run { program, shell: true, refresh: true, .. }) if program == "open -a Safari"));
        assert_eq!(c.items[2].action, Some(ItemAction::Copy("secret-id".into())));
        assert_eq!(c.items[4].title, "5");
        let c = parse_output(r#"{"title":"T","value":12,"progress":40}"#, OutputFormat::Json).unwrap();
        assert_eq!((c.value.as_deref(), c.progress), (Some("12"), Some(0.4)));
        let c = parse_output(r#"{"title":"Only items","items":["a","b"]}"#, OutputFormat::Json).unwrap();
        assert_eq!(c.style, Style::List);
        let c = parse_output(r#"[{"title":"x"},{"name":"y","value":3}]"#, OutputFormat::Auto).unwrap();
        assert_eq!((c.style, c.items[1].title.as_str(), c.items[1].subtitle.as_str()), (Style::List, "y", "3"));
        assert!(parse_output("{oops", OutputFormat::Json).unwrap_err().starts_with("Invalid JSON"));
        assert!(parse_output("42", OutputFormat::Json).is_err());
        // Auto falls back to text when JSON-looking output is broken.
        assert_eq!(parse_output("{oops", OutputFormat::Auto).unwrap().title, "{oops");
    }

    #[test]
    fn actions_colors_and_status() {
        assert_eq!(parse_action(&serde_json::json!("refresh")), Some(ItemAction::Refresh));
        assert_eq!(parse_action(&serde_json::json!("nothing")), None);
        assert_eq!(parse_action(&serde_json::json!({"url": "https://a.b"})), Some(ItemAction::OpenUrl("https://a.b".into())));
        assert_eq!(parse_color("#fff"), Some(Rgba(1.0, 1.0, 1.0, 1.0)));
        assert_eq!(parse_color("#ff000080").unwrap().3, 128.0 / 255.0);
        assert_eq!(parse_color("black,white"), Some(Rgba(1.0, 1.0, 1.0, 1.0)));
        assert_eq!(parse_color("Red"), parse_color("red"));
        assert!(parse_color("#12").is_none() && parse_color("chartreuse-ish").is_none());
        let ok = Status::Loading.next(Ok(parse_text("a")), 1.0);
        let err = ok.clone().next(Err("boom".into()), 2.0);
        assert_eq!(err.content().unwrap().title, "a");
        let err2 = err.next(Err("again".into()), 3.0);
        assert!(matches!(&err2, Status::Error { message, stale: Some(_), .. } if message == "again"));
        assert!(Status::Loading.next(Err("x".into()), 0.0).content().is_none());
        assert_eq!(fmt_value(1.23456), "1.23");
        assert_eq!(fmt_value(2.5), "2.5");
        assert_eq!(fmt_value(1234.56), "1234.6");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}ok"), "ok");
    }
}
