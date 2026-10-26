//! Web / JSON widget logic: URL policy, headers, a small JSONPath subset,
//! the `{{path | filter}}` template engine and response → card content.
//! The request itself is made by the platform layer.

use serde_json::Value;

use super::catalog::WebSettings;
use super::content::{fmt_value, Content, Item, ItemAction, Style};

/// HTTPS only unless `allow_http`; no credentials in the URL.
pub fn check_url(url: &str, allow_http: bool) -> Result<(), String> {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    let rest = if let Some(r) = lower.strip_prefix("https://") {
        r
    } else if let Some(r) = lower.strip_prefix("http://") {
        if !allow_http {
            return Err("must use https:// (turn on \u{201c}Allow plain HTTP\u{201d} for http://)".into());
        }
        r
    } else {
        return Err("must start with https://".into());
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return Err("has no host".into());
    }
    if host.contains('@') {
        return Err("must not contain a user name or password (use the token field)".into());
    }
    if u.contains(char::is_whitespace) {
        return Err("must not contain spaces".into());
    }
    Ok(())
}

/// `"Name: value"` → (name, value). Names are HTTP tokens.
pub fn parse_header(s: &str) -> Option<(String, String)> {
    let (n, v) = s.split_once(':')?;
    let n = n.trim();
    let ok = !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
    ok.then(|| (n.to_string(), v.trim().to_string()))
}

/// Request headers from the settings (the token header last, so it wins).
pub fn request_headers(s: &WebSettings) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = s.headers.iter().filter_map(|l| parse_header(l)).collect();
    if !h.iter().any(|(n, _)| n.eq_ignore_ascii_case("accept")) {
        h.push(("Accept".into(), "application/json, text/plain;q=0.9, */*;q=0.5".into()));
    }
    if !s.token.trim().is_empty() {
        let name = if s.token_header.trim().is_empty() { "Authorization" } else { s.token_header.trim() };
        h.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        h.push((name.to_string(), format!("{}{}", s.token_prefix, s.token.trim())));
    }
    h
}

// ----- JSONPath-lite -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Key(String),
    /// Negative counts from the end.
    Index(i64),
    /// `[*]` / `.*`: every element / value.
    All,
    /// `.length()` / `.length` on arrays, objects and strings (when there's
    /// no such key).
    Length,
}

/// A path like `data.items[0].price`, `$.list[-1]["key with spaces"]`,
/// `items[*].name`, `prices.length()`.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonPath(pub Vec<Seg>);

impl JsonPath {
    pub fn parse(s: &str) -> Result<JsonPath, String> {
        let c: Vec<char> = s.trim().chars().collect();
        let mut i = 0;
        let mut segs = vec![];
        if c.first() == Some(&'$') {
            i = 1;
        }
        let ident_end = |c: &[char], mut j: usize| {
            while j < c.len() && c[j] != '.' && c[j] != '[' {
                j += 1;
            }
            j
        };
        let mut need_seg = !c.is_empty() && i == 0;
        while i < c.len() {
            match c[i] {
                '.' => {
                    i += 1;
                    if i >= c.len() {
                        return Err("ends with \u{201c}.\u{201d}".into());
                    }
                    need_seg = true;
                }
                '[' => {
                    let close = c[i..].iter().position(|x| *x == ']').map(|p| p + i).ok_or("missing \u{201c}]\u{201d}")?;
                    let inner: String = c[i + 1..close].iter().collect();
                    let inner = inner.trim();
                    let seg = if inner == "*" {
                        Seg::All
                    } else if (inner.starts_with('"') && inner.ends_with('"') || inner.starts_with('\'') && inner.ends_with('\'')) && inner.len() >= 2 {
                        Seg::Key(inner[1..inner.len() - 1].to_string())
                    } else {
                        Seg::Index(inner.parse().map_err(|_| format!("\u{201c}[{inner}]\u{201d} isn't an index"))?)
                    };
                    segs.push(seg);
                    i = close + 1;
                    need_seg = false;
                    continue;
                }
                _ => {
                    if !need_seg {
                        return Err(format!("unexpected \u{201c}{}\u{201d}", c[i]));
                    }
                }
            }
            if i < c.len() && c[i] != '[' && c[i] != '.' {
                let e = ident_end(&c, i);
                let key: String = c[i..e].iter().collect();
                let key = key.trim().to_string();
                segs.push(match key.as_str() {
                    "*" => Seg::All,
                    "length()" => Seg::Length,
                    _ => Seg::Key(key),
                });
                i = e;
                need_seg = false;
            }
        }
        Ok(JsonPath(segs))
    }

    /// Values at the path (several after a wildcard).
    pub fn select(&self, root: &Value) -> Vec<Value> {
        let mut cur: Vec<Value> = vec![root.clone()];
        let mut spread = false;
        for s in &self.0 {
            let mut next = vec![];
            for v in &cur {
                match s {
                    Seg::Key(k) => match v {
                        Value::Object(o) => {
                            if let Some(x) = o.get(k) {
                                next.push(x.clone());
                            } else if k == "length" {
                                next.push(Value::from(o.len()));
                            }
                        }
                        Value::Array(a) if k == "length" => next.push(Value::from(a.len())),
                        Value::String(t) if k == "length" => next.push(Value::from(t.chars().count())),
                        _ => {}
                    },
                    Seg::Index(n) => {
                        if let Value::Array(a) = v {
                            let idx = if *n < 0 { a.len() as i64 + n } else { *n };
                            if idx >= 0 && (idx as usize) < a.len() {
                                next.push(a[idx as usize].clone());
                            }
                        }
                    }
                    Seg::All => {
                        spread = true;
                        match v {
                            Value::Array(a) => next.extend(a.iter().cloned()),
                            Value::Object(o) => next.extend(o.values().cloned()),
                            _ => {}
                        }
                    }
                    Seg::Length => match v {
                        Value::Array(a) => next.push(Value::from(a.len())),
                        Value::Object(o) => next.push(Value::from(o.len())),
                        Value::String(t) => next.push(Value::from(t.chars().count())),
                        _ => {}
                    },
                }
            }
            cur = next;
        }
        let _ = spread;
        cur
    }

    /// The single value at the path (an array after a wildcard).
    pub fn get(&self, root: &Value) -> Option<Value> {
        let v = self.select(root);
        if self.0.contains(&Seg::All) {
            Some(Value::Array(v))
        } else {
            v.into_iter().next()
        }
    }
}

// ----- templates -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Filter {
    pub name: String,
    pub arg: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Lit(String),
    Var { path: JsonPath, filters: Vec<Filter> },
}

/// `"{{data.price | round:2}} {{data.currency | upper}}"`. `{{{{` is a
/// literal `{{`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Template(pub Vec<Part>);

pub const FILTERS: &[&str] = &["round", "fixed", "int", "upper", "lower", "trim", "default", "join", "len", "percent", "bytes", "abs", "date", "time", "prefix", "suffix", "first", "last"];

fn parse_filter(s: &str) -> Result<Filter, String> {
    let s = s.trim();
    let (name, arg) = match s.split_once(':') {
        Some((n, a)) => {
            let a = a.trim();
            let a = if a.len() >= 2 && (a.starts_with('"') && a.ends_with('"') || a.starts_with('\'') && a.ends_with('\'')) { &a[1..a.len() - 1] } else { a };
            (n.trim(), Some(a.to_string()))
        }
        None => (s, None),
    };
    if !FILTERS.contains(&name) {
        return Err(format!("unknown filter \u{201c}{name}\u{201d}"));
    }
    if matches!(name, "round" | "fixed") {
        if let Some(a) = &arg {
            a.parse::<u32>().map_err(|_| format!("{name} needs a number of decimals"))?;
        }
    }
    Ok(Filter { name: name.to_string(), arg })
}

impl Template {
    pub fn parse(s: &str) -> Result<Template, String> {
        let mut parts = vec![];
        let mut lit = String::new();
        let mut rest = s;
        while let Some(a) = rest.find("{{") {
            if rest[a..].starts_with("{{{{") {
                lit.push_str(&rest[..a]);
                lit.push_str("{{");
                rest = &rest[a + 4..];
                continue;
            }
            lit.push_str(&rest[..a]);
            let after = &rest[a + 2..];
            let b = after.find("}}").ok_or("a \u{201c}{{\u{201d} is not closed")?;
            let inner = &after[..b];
            let mut pieces = inner.split('|');
            let p = pieces.next().unwrap_or("").trim();
            if p.is_empty() {
                return Err("empty {{ }}".into());
            }
            let path = JsonPath::parse(p)?;
            let filters = pieces.map(parse_filter).collect::<Result<Vec<_>, _>>()?;
            if !lit.is_empty() {
                parts.push(Part::Lit(std::mem::take(&mut lit)));
            }
            parts.push(Part::Var { path, filters });
            rest = &after[b + 2..];
        }
        lit.push_str(rest);
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        Ok(Template(parts))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Render against `root`; missing values render empty (or their
    /// `default:` filter).
    pub fn render(&self, root: &Value, utc_offset: f64) -> String {
        let mut out = String::new();
        for p in &self.0 {
            match p {
                Part::Lit(s) => out.push_str(s),
                Part::Var { path, filters } => {
                    let mut v = path.get(root);
                    for f in filters {
                        v = apply_filter(v, f, utc_offset);
                    }
                    out.push_str(&to_text(v.as_ref()));
                }
            }
        }
        out
    }

    /// The first variable's path (to pick a number for gauges).
    pub fn first_path(&self) -> Option<&JsonPath> {
        self.0.iter().find_map(|p| if let Part::Var { path, .. } = p { Some(path) } else { None })
    }
}

/// Text for a JSON value: strings as they are, numbers compact, arrays
/// joined with ", ", null / missing as "".
pub fn to_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.as_f64().map(fmt_value).unwrap_or_else(|| n.to_string()),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Array(a)) => a.iter().map(|x| to_text(Some(x))).collect::<Vec<_>>().join(", "),
        Some(o @ Value::Object(_)) => o.to_string(),
    }
}

pub fn as_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_end_matches('%').replace(',', "").parse().ok(),
        Value::Bool(b) => Some(*b as u8 as f64),
        _ => None,
    }
}

fn apply_filter(v: Option<Value>, f: &Filter, utc_offset: f64) -> Option<Value> {
    let arg = f.arg.as_deref();
    let num = |v: &Option<Value>| v.as_ref().and_then(as_number);
    let s = |v: &Option<Value>| to_text(v.as_ref());
    match f.name.as_str() {
        "default" => match &v {
            None | Some(Value::Null) => Some(Value::String(arg.unwrap_or("").to_string())),
            Some(Value::String(t)) if t.is_empty() => Some(Value::String(arg.unwrap_or("").to_string())),
            _ => v,
        },
        "round" | "fixed" => {
            let n = num(&v)?;
            let d: usize = arg.and_then(|a| a.parse().ok()).unwrap_or(0);
            Some(Value::String(format!("{n:.d$}")))
        }
        "int" => num(&v).map(|n| Value::String(format!("{}", n.trunc() as i64))),
        "abs" => num(&v).map(|n| Value::String(fmt_value(n.abs()))),
        "upper" => Some(Value::String(s(&v).to_uppercase())),
        "lower" => Some(Value::String(s(&v).to_lowercase())),
        "trim" => Some(Value::String(s(&v).trim().to_string())),
        "prefix" => Some(Value::String(if s(&v).is_empty() { String::new() } else { format!("{}{}", arg.unwrap_or(""), s(&v)) })),
        "suffix" => Some(Value::String(if s(&v).is_empty() { String::new() } else { format!("{}{}", s(&v), arg.unwrap_or("")) })),
        "len" => Some(Value::from(match &v {
            Some(Value::Array(a)) => a.len(),
            Some(Value::Object(o)) => o.len(),
            Some(Value::String(t)) => t.chars().count(),
            _ => 0,
        })),
        "first" => match v {
            Some(Value::Array(a)) => a.into_iter().next(),
            other => other,
        },
        "last" => match v {
            Some(Value::Array(a)) => a.into_iter().last(),
            other => other,
        },
        "join" => match v {
            Some(Value::Array(a)) => Some(Value::String(a.iter().map(|x| to_text(Some(x))).collect::<Vec<_>>().join(arg.unwrap_or(", ")))),
            other => other,
        },
        "percent" => {
            let n = num(&v)?;
            // 0..1 fractions become percentages; larger numbers are already percent.
            let p = if n.abs() <= 1.0 { n * 100.0 } else { n };
            let d: usize = arg.and_then(|a| a.parse().ok()).unwrap_or(0);
            Some(Value::String(format!("{p:.d$}%")))
        }
        "bytes" => num(&v).map(|n| Value::String(format_bytes(n))),
        "date" | "time" => {
            let n = num(&v).or_else(|| v.as_ref().and_then(|x| x.as_str()).and_then(parse_iso8601))?;
            // Milliseconds since the epoch are common too.
            let secs = if n > 1e11 { n / 1000.0 } else { n };
            Some(Value::String(format_time(secs, utc_offset, f.name == "date", arg)))
        }
        _ => v,
    }
}

pub fn format_bytes(n: f64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n;
    let mut i = 0;
    while v.abs() >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", v as i64)
    } else {
        format!("{} {}", if v.abs() >= 100.0 { format!("{v:.0}") } else { format!("{v:.1}") }, units[i])
    }
}

/// `2024-05-01T12:30:00Z` / `+02:00` / `2024-05-01` → unix seconds.
pub fn parse_iso8601(s: &str) -> Option<f64> {
    let s = s.trim();
    let (date, rest) = if s.len() >= 10 { s.split_at(10) } else { return None };
    let mut d = date.split('-');
    let (y, m, dd): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&dd) {
        return None;
    }
    let days = days_from_civil(y, m, dd);
    let mut secs = days as f64 * 86_400.0;
    let rest = rest.trim_start_matches(['T', ' ']);
    if rest.is_empty() {
        return Some(secs);
    }
    let tz_at = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (time, tz) = rest.split_at(tz_at);
    let mut t = time.split(':');
    let h: f64 = t.next()?.parse().ok()?;
    let mi: f64 = t.next().unwrap_or("0").parse().ok()?;
    let se: f64 = t.next().unwrap_or("0").parse().ok()?;
    secs += h * 3600.0 + mi * 60.0 + se;
    if tz.starts_with('+') || tz.starts_with('-') {
        let sign = if tz.starts_with('-') { -1.0 } else { 1.0 };
        let z = tz[1..].replace(':', "");
        let (hh, mm) = z.split_at(z.len().min(2));
        let off = hh.parse::<f64>().ok()? * 3600.0 + mm.parse::<f64>().unwrap_or(0.0) * 60.0;
        secs -= sign * off;
    }
    Some(secs)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "14:05" (time), "May 1" / "May 1, 14:05" (date). `arg` "short" drops the time of a date.
fn format_time(secs: f64, utc_offset: f64, date: bool, arg: Option<&str>) -> String {
    let local = secs + utc_offset;
    let mins = (local / 60.0).floor() as i64;
    let (h, m) = ((mins / 60).rem_euclid(24), mins.rem_euclid(60));
    let t = format!("{h}:{m:02}");
    if !date {
        return t;
    }
    let (_, mo, d) = super::calendar::civil_date(secs, utc_offset);
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let ds = format!("{} {}", M[(mo as usize).saturating_sub(1).min(11)], d);
    if arg == Some("short") {
        ds
    } else {
        format!("{ds}, {t}")
    }
}

// ----- response → content --------------------------------------------------------

/// An http(s) URL from fetched data, or nothing (no `file:`, app schemes
/// or commands from the network).
pub fn safe_link(u: &str) -> Option<String> {
    let t = u.trim();
    let l = t.to_ascii_lowercase();
    ((l.starts_with("https://") || l.starts_with("http://")) && t.len() > 8 && !t.contains(char::is_whitespace)).then(|| t.to_string())
}

/// Turn a response body into card content using the widget's templates.
/// Non-JSON bodies are available as `{{body}}` (and `{{.}}`-less templates
/// show the first line).
pub fn to_content(body: &str, s: &WebSettings, utc_offset: f64) -> Result<Content, String> {
    let root: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            // Plain text: {{body}} / {{text}} work, anything else shows the text.
            let first = body.trim().lines().next().unwrap_or("").trim().to_string();
            serde_json::json!({ "body": body.trim(), "text": first })
        }
    };
    let tpl = |t: &str| Template::parse(t).map_err(|e| format!("Template: {e}"));
    let value_t = tpl(&s.template)?;
    let mut c = Content { title: s.title.clone(), symbol: Some(s.symbol.clone()).filter(|x| !x.is_empty()), ..Default::default() };
    let value = if value_t.is_empty() { to_text(root.get("text").or(Some(&root))) } else { value_t.render(&root, utc_offset) };
    c.subtitle = tpl(&s.subtitle)?.render(&root, utc_offset);
    if !s.value_path.trim().is_empty() {
        let p = JsonPath::parse(&s.value_path).map_err(|e| format!("Gauge value: {e}"))?;
        let n = p.get(&root).as_ref().and_then(as_number).ok_or_else(|| format!("No number at \u{201c}{}\u{201d}", s.value_path.trim()))?;
        c.number = Some(n);
        if s.max > s.min {
            c.progress = Some(((n - s.min) / (s.max - s.min)).clamp(0.0, 1.0));
        }
    }
    if !s.list_path.trim().is_empty() {
        let p = JsonPath::parse(&s.list_path).map_err(|e| format!("List: {e}"))?;
        let item_t = tpl(&s.item_template)?;
        let arr = match p.get(&root) {
            Some(Value::Array(a)) => a,
            Some(Value::Object(o)) => o.into_iter().map(|(k, v)| serde_json::json!({"key": k, "value": v})).collect(),
            _ => return Err(format!("No list at \u{201c}{}\u{201d}", s.list_path.trim())),
        };
        c.items = arr
            .iter()
            .take(50)
            .map(|it| {
                let title = if item_t.is_empty() { to_text(Some(it)) } else { item_t.render(it, utc_offset) };
                // Fetched content is untrusted: only http(s) links, never commands.
                let action = it.get("url").or_else(|| it.get("html_url")).or_else(|| it.get("link")).and_then(|v| v.as_str()).and_then(safe_link).map(ItemAction::OpenUrl);
                Item { title, action, ..Default::default() }
            })
            .collect();
    }
    if !s.open_url.trim().is_empty() {
        let u = tpl(&s.open_url)?.render(&root, utc_offset);
        c.action = safe_link(&u).map(ItemAction::OpenUrl);
    }
    if c.items.is_empty() {
        c.style = Style::Number;
        c.value = Some(value);
    } else {
        c.style = Style::List;
        if !value.trim().is_empty() {
            c.value = Some(value);
        }
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn url_policy() {
        assert!(check_url("https://api.github.com/zen", false).is_ok());
        assert!(check_url("HTTPS://X.Y", false).is_ok());
        assert!(check_url("http://192.168.0.2:8080/x", false).is_err());
        assert!(check_url("http://192.168.0.2:8080/x", true).is_ok());
        assert!(check_url("ftp://x.y", true).is_err());
        assert!(check_url("file:///etc/passwd", true).is_err());
        assert!(check_url("https://user:pw@x.y/", false).is_err());
        assert!(check_url("https:///path", false).is_err());
        assert!(check_url("https://x.y/a b", false).is_err());
    }

    #[test]
    fn headers() {
        assert_eq!(parse_header("X-Api-Key:  abc:def "), Some(("X-Api-Key".into(), "abc:def".into())));
        assert_eq!(parse_header("bad header: x"), None);
        assert_eq!(parse_header("nocolon"), None);
        let s = WebSettings { headers: vec!["Authorization: old".into(), "X-A: 1".into()], token: " t0k ".into(), ..Default::default() };
        let h = request_headers(&s);
        assert!(h.contains(&("Authorization".into(), "Bearer t0k".into())));
        assert!(!h.iter().any(|(_, v)| v == "old"));
        assert!(h.iter().any(|(n, _)| n == "Accept"));
        let s2 = WebSettings { token: "k".into(), token_header: "X-Token".into(), token_prefix: String::new(), ..Default::default() };
        assert!(request_headers(&s2).contains(&("X-Token".into(), "k".into())));
        assert!(request_headers(&WebSettings::default()).iter().all(|(n, _)| n != "Authorization"));
    }

    fn doc() -> Value {
        json!({
            "data": {"price": 67212.456, "currency": "usd", "change": -0.0123, "items": [
                {"name": "a", "v": 1, "url": "https://a.example"}, {"name": "b", "v": 2}, {"name": "c", "v": 3}
            ], "key with space": "yes", "ts": 1714566600, "iso": "2024-05-01T12:30:00+02:00", "empty": "", "nothing": null},
            "list": [10, 20, 30],
            "ok": true
        })
    }

    #[test]
    fn json_paths() {
        let d = doc();
        let g = |p: &str| JsonPath::parse(p).unwrap().get(&d);
        assert_eq!(g("data.price"), Some(json!(67212.456)));
        assert_eq!(g("$.data.price"), Some(json!(67212.456)));
        assert_eq!(g("data.items[1].name"), Some(json!("b")));
        assert_eq!(g("data.items[-1].v"), Some(json!(3)));
        assert_eq!(g("data.items[9]"), None);
        assert_eq!(g("data[\"key with space\"]"), Some(json!("yes")));
        assert_eq!(g("data['key with space']"), Some(json!("yes")));
        assert_eq!(g("data.items[*].name"), Some(json!(["a", "b", "c"])));
        assert_eq!(g("data.items.*.v"), Some(json!([1, 2, 3])));
        assert_eq!(g("list.length"), Some(json!(3)));
        assert_eq!(g("list.length()"), Some(json!(3)));
        assert_eq!(g("list[0]"), Some(json!(10)));
        assert_eq!(g("$"), Some(d.clone()));
        assert_eq!(g("missing.deep.path"), None);
        assert_eq!(JsonPath::parse("a[").unwrap_err(), "missing \u{201c}]\u{201d}");
        assert!(JsonPath::parse("a[x]").is_err());
        assert!(JsonPath::parse("a.").is_err());
        assert!(JsonPath::parse("a]b").is_err() || JsonPath::parse("a]b").is_ok());
    }

    #[test]
    fn templates_and_filters() {
        let d = doc();
        let r = |t: &str| Template::parse(t).unwrap().render(&d, 0.0);
        assert_eq!(r("{{data.price | round:2}} {{data.currency | upper}}"), "67212.46 USD");
        assert_eq!(r("{{ data.price|int }}"), "67212");
        assert_eq!(r("{{data.change | percent:1}}"), "-1.2%");
        assert_eq!(r("{{data.items[*].name | join:/}}"), "a/b/c");
        assert_eq!(r("{{data.items | len}} items"), "3 items");
        assert_eq!(r("{{data.nothing | default:\"n/a\"}}|{{data.empty | default:-}}|{{data.missing}}"), "n/a|-|");
        assert_eq!(r("{{list}}"), "10, 20, 30");
        assert_eq!(r("{{ok}} {{list | first}}-{{list | last}}"), "true 10-30");
        assert_eq!(r("{{data.ts | time}}"), "12:30");
        assert_eq!(r("{{data.ts | date}}"), "May 1, 12:30");
        assert_eq!(r("{{data.iso | time}}"), "10:30");
        assert_eq!(r("{{data.iso | date:short}}"), "May 1");
        assert_eq!(r("{{list[2] | bytes}} {{data.price | bytes}}"), "30 B 67.2 KB");
        assert_eq!(r("{{data.change | abs}}"), "0.01");
        assert_eq!(r("{{data.currency | prefix:\"in \"}}{{data.missing | prefix:x}}"), "in usd");
        assert_eq!(r("literal {{{{ braces"), "literal {{ braces");
        assert_eq!(r("no vars"), "no vars");
        assert!(Template::parse("{{a").is_err());
        assert!(Template::parse("{{}}").is_err());
        assert!(Template::parse("{{a | frobnicate}}").unwrap_err().contains("frobnicate"));
        assert!(Template::parse("{{a | round:x}}").is_err());
        assert!(Template::parse("").unwrap().is_empty());
        assert_eq!(Template::parse("x {{a.b}} {{c}}").unwrap().first_path(), Some(&JsonPath::parse("a.b").unwrap()));
    }

    #[test]
    fn dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(parse_iso8601("2024-05-01T12:30:00Z"), Some(1_714_566_600.0));
        assert_eq!(parse_iso8601("2024-05-01T14:30:00+02:00"), Some(1_714_566_600.0));
        assert_eq!(parse_iso8601("2024-05-01T07:30:00.000-05:00"), Some(1_714_566_600.0));
        assert_eq!(parse_iso8601("2024-05-01"), Some(1_714_521_600.0));
        assert_eq!(parse_iso8601("yesterday"), None);
        assert_eq!(format_bytes(999.0), "999 B");
        assert_eq!(format_bytes(1_500_000.0), "1.5 MB");
    }

    #[test]
    fn responses_to_content() {
        let body = doc().to_string();
        let s = WebSettings {
            title: "BTC".into(),
            template: "${{data.price | round:0}}".into(),
            subtitle: "{{data.change | percent:2}} today".into(),
            value_path: "data.items[1].v".into(),
            min: 0.0,
            max: 4.0,
            open_url: "https://example.com/{{data.currency}}".into(),
            ..Default::default()
        };
        let c = to_content(&body, &s, 0.0).unwrap();
        assert_eq!((c.title.as_str(), c.value.as_deref(), c.subtitle.as_str()), ("BTC", Some("$67212"), "-1.23% today"));
        assert_eq!((c.number, c.progress), (Some(2.0), Some(0.5)));
        assert_eq!(c.action, Some(ItemAction::OpenUrl("https://example.com/usd".into())));
        assert_eq!(c.style, Style::Number);
        // Lists.
        let l = WebSettings { list_path: "data.items".into(), item_template: "{{name}} = {{v}}".into(), ..Default::default() };
        let c = to_content(&body, &l, 0.0).unwrap();
        assert_eq!(c.style, Style::List);
        assert_eq!(c.items.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), vec!["a = 1", "b = 2", "c = 3"]);
        assert_eq!(c.items[0].action, Some(ItemAction::OpenUrl("https://a.example".into())));
        let obj = WebSettings { list_path: "data".into(), item_template: "{{key}}".into(), ..Default::default() };
        assert!(to_content(&body, &obj, 0.0).unwrap().items.iter().any(|i| i.title == "price"));
        // Untrusted links: only http(s).
        let evil = r#"{"items":[{"name":"a","url":"run:rm -rf ~"},{"name":"b","url":"file:///etc/passwd"},{"name":"c","url":"https://ok.example"}]}"#;
        let c = to_content(evil, &WebSettings { list_path: "items".into(), item_template: "{{name}}".into(), open_url: "{{items[1].url}}".into(), ..Default::default() }, 0.0).unwrap();
        assert_eq!(c.items.iter().map(|i| i.action.clone()).collect::<Vec<_>>(), vec![None, None, Some(ItemAction::OpenUrl("https://ok.example".into()))]);
        assert_eq!(c.action, None);
        // Errors.
        assert!(to_content(&body, &WebSettings { list_path: "nope".into(), ..Default::default() }, 0.0).unwrap_err().contains("No list"));
        assert!(to_content(&body, &WebSettings { value_path: "data.currency".into(), ..Default::default() }, 0.0).unwrap_err().contains("No number"));
        assert!(to_content(&body, &WebSettings { template: "{{x".into(), ..Default::default() }, 0.0).unwrap_err().starts_with("Template"));
        // Plain text bodies (api.github.com/zen).
        let c = to_content("Design for failure.\n", &WebSettings { template: "{{text}}".into(), ..Default::default() }, 0.0).unwrap();
        assert_eq!(c.value.as_deref(), Some("Design for failure."));
        let c = to_content("Keep it logically awesome.", &WebSettings::default(), 0.0).unwrap();
        assert_eq!(c.value.as_deref(), Some("Keep it logically awesome."));
    }
}
