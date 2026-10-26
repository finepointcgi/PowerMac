//! Import espanso match files (a simple subset of YAML).
//!
//! Supported: a top-level `matches:` list whose items have `trigger:` or
//! `triggers: [..]`, `replace:` (plain, quoted, or `|` / `>` block scalars),
//! `word: true`, `propagate_case: true`, and `vars:` of `type: date`
//! (`params: {format: ...}`) / `type: clipboard`. `{{name}}` references to
//! those become `{date:...}` / `{clipboard}`, `$|$` becomes `{cursor}`.
//! Everything else (forms, shell, regex triggers, `image_path`, `html`,
//! global vars, ...) is skipped with a warning.

use std::collections::HashMap;

use crate::config::Snippet;

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Str(String),
    List(Vec<Val>),
    Map(Vec<(String, Val)>),
}

impl Val {
    fn get(&self, k: &str) -> Option<&Val> {
        match self {
            Val::Map(m) => m.iter().find(|(key, _)| key == k).map(|(_, v)| v),
            _ => None,
        }
    }
    fn str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
    fn truthy(&self) -> bool {
        matches!(self.str(), Some("true" | "yes" | "True" | "on"))
    }
}

struct Line<'a> {
    indent: usize,
    text: &'a str,
}

fn strip_comment(s: &str) -> &str {
    // A '#' starts a comment outside quotes when preceded by whitespace.
    let mut quote: Option<char> = None;
    let mut prev_ws = true;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && prev_ws => return s[..i].trim_end(),
            None => {}
        }
        prev_ws = c.is_whitespace();
    }
    s.trim_end()
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::new();
        let mut it = inner.chars();
        while let Some(c) = it.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let h: String = it.by_ref().take(4).collect();
                    if let Some(c) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                        out.push(c);
                    }
                }
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        }
        out
    } else if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].replace("''", "'")
    } else {
        s.to_string()
    }
}

/// Split a flow collection body (`a, "b, c", d`) at top-level commas.
fn split_flow(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0;
    for c in s.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '[' | '{' => {
                    depth += 1;
                    cur.push(c);
                }
                ']' | '}' => {
                    depth -= 1;
                    cur.push(c);
                }
                ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            },
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.into_iter().map(|s| s.trim().to_string()).collect()
}

fn scalar(s: &str) -> Val {
    let t = s.trim();
    if t.starts_with('[') && t.ends_with(']') {
        return Val::List(
            split_flow(&t[1..t.len() - 1])
                .iter()
                .map(|x| scalar(x))
                .collect(),
        );
    }
    if t.starts_with('{') && t.ends_with('}') {
        let mut m = Vec::new();
        for kv in split_flow(&t[1..t.len() - 1]) {
            if let Some((k, v)) = split_key(&kv) {
                m.push((k, scalar(v)));
            }
        }
        return Val::Map(m);
    }
    Val::Str(unquote(t))
}

/// `key: rest` (key may be quoted). Not for `- ` items.
fn split_key(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    let (key, rest) = if s.starts_with('"') || s.starts_with('\'') {
        let q = s.chars().next()?;
        let end = s[1..].find(q)? + 1;
        (unquote(&s[..=end]), s[end + 1..].trim_start())
    } else {
        let i = s
            .find(": ")
            .or_else(|| s.strip_suffix(':').map(|x| x.len()))?;
        (s[..i].trim().to_string(), &s[i..])
    };
    let rest = rest.strip_prefix(':')?;
    Some((key, rest))
}

struct Parser<'a> {
    lines: Vec<Line<'a>>,
    raw: Vec<&'a str>,
    /// index into `raw` for each entry in `lines`
    raw_idx: Vec<usize>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        let raw: Vec<&str> = src.lines().collect();
        let mut lines = Vec::new();
        let mut raw_idx = Vec::new();
        for (i, l) in raw.iter().enumerate() {
            let t = strip_comment(l);
            if t.trim().is_empty() || t.trim() == "---" {
                continue;
            }
            let indent = t.len() - t.trim_start().len();
            lines.push(Line {
                indent,
                text: t.trim_start(),
            });
            raw_idx.push(i);
        }
        Parser {
            lines,
            raw,
            raw_idx,
            pos: 0,
        }
    }

    /// A block scalar (`|`, `>`, with `-`/`+` chomping) whose header is on
    /// line `pos - 1` at indent `parent`.
    fn block_scalar(&mut self, header: &str, parent: usize) -> Val {
        let fold = header.starts_with('>');
        let keep = header.contains('+');
        let strip = header.contains('-');
        let start_raw = self.raw_idx[self.pos - 1] + 1;
        let mut body: Vec<&str> = Vec::new();
        let mut end_raw = start_raw;
        let mut indent: Option<usize> = None;
        for (i, l) in self.raw.iter().enumerate().skip(start_raw) {
            let ind = l.len() - l.trim_start().len();
            if l.trim().is_empty() {
                body.push("");
                end_raw = i + 1;
                continue;
            }
            if ind <= parent {
                break;
            }
            let ind0 = *indent.get_or_insert(ind);
            body.push(if l.len() >= ind0 {
                &l[ind0.min(ind)..]
            } else {
                l.trim_start()
            });
            end_raw = i + 1;
        }
        while self.pos < self.lines.len() && self.raw_idx[self.pos] < end_raw {
            self.pos += 1;
        }
        // trailing blank lines
        let mut trailing = 0;
        while body.last() == Some(&"") {
            body.pop();
            trailing += 1;
        }
        let mut s = if fold {
            let mut out = String::new();
            for (i, l) in body.iter().enumerate() {
                if i > 0 {
                    out.push(if l.is_empty() || body[i - 1].is_empty() {
                        '\n'
                    } else {
                        ' '
                    });
                }
                out.push_str(l);
            }
            out
        } else {
            body.join("\n")
        };
        if !strip {
            s.push('\n');
            if keep {
                s.push_str(&"\n".repeat(trailing));
            }
        }
        Val::Str(s)
    }

    /// Parse the value after `key:` (rest of the line) for a key at `indent`.
    fn value(&mut self, rest: &str, indent: usize) -> Val {
        let r = rest.trim();
        if r.starts_with('|') || r.starts_with('>') {
            return self.block_scalar(r, indent);
        }
        if !r.is_empty() {
            return scalar(r);
        }
        // nested block
        match self.lines.get(self.pos) {
            Some(l) if l.indent > indent || (l.indent == indent && l.text.starts_with("- ")) => {
                let child = l.indent;
                self.node(child)
            }
            _ => Val::Str(String::new()),
        }
    }

    fn node(&mut self, indent: usize) -> Val {
        let Some(first) = self.lines.get(self.pos) else {
            return Val::Str(String::new());
        };
        if first.text.starts_with("- ") || first.text == "-" {
            let mut items = Vec::new();
            while let Some(l) = self.lines.get(self.pos) {
                if l.indent != indent || !(l.text.starts_with("- ") || l.text == "-") {
                    break;
                }
                let after = l.text[1..].trim_start();
                let item_indent = indent + (l.text.len() - after.len());
                self.pos += 1;
                if after.is_empty() {
                    items.push(self.value("", indent));
                } else if split_key(after).is_some()
                    && !after.starts_with('[')
                    && !after.starts_with('{')
                {
                    // "- key: value" starts a map at item_indent
                    items.push(self.map(item_indent, Some(after)));
                } else {
                    items.push(scalar(after));
                }
            }
            Val::List(items)
        } else {
            self.map(indent, None)
        }
    }

    fn map(&mut self, indent: usize, first: Option<&str>) -> Val {
        let mut m = Vec::new();
        if let Some(f) = first {
            if let Some((k, rest)) = split_key(f) {
                let v = self.value(rest, indent);
                m.push((k, v));
            }
        }
        while let Some(l) = self.lines.get(self.pos) {
            if l.indent != indent || l.text.starts_with("- ") {
                break;
            }
            let text = l.text;
            self.pos += 1;
            if let Some((k, rest)) = split_key(text) {
                let v = self.value(rest, indent);
                m.push((k, v));
            }
        }
        Val::Map(m)
    }
}

fn parse(src: &str) -> Val {
    let mut p = Parser::new(src);
    let indent = p.lines.first().map_or(0, |l| l.indent);
    p.node(indent)
}

/// Escape our variable syntax in literal text.
fn escape_braces(s: &str) -> String {
    s.replace('{', "{{").replace('}', "}}")
}

/// Convert espanso `{{var}}` / `$|$` to our syntax.
fn convert_replace(
    rep: &str,
    vars: &HashMap<String, String>,
    warn: &mut Vec<String>,
    trig: &str,
) -> String {
    let mut out = String::new();
    let mut rest = rep;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("$|$") {
            out.push_str("{cursor}");
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix("{{") {
            if let Some(end) = r.find("}}") {
                let name = r[..end].trim();
                match vars.get(name) {
                    Some(v) => out.push_str(v),
                    None => {
                        warn.push(format!(
                            "{trig}: unsupported variable {{{{{name}}}}} kept as text"
                        ));
                        out.push_str(&escape_braces(&rest[..end + 4]));
                    }
                }
                rest = &r[end + 2..];
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        out.push_str(&escape_braces(&c.to_string()));
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Parse an espanso match file. Returns the snippets plus warnings about
/// what was skipped.
pub fn import(yaml: &str) -> (Vec<Snippet>, Vec<String>) {
    let doc = parse(yaml);
    let mut out = Vec::new();
    let mut warn = Vec::new();
    // global vars apply to every match
    let mut global: HashMap<String, String> = HashMap::new();
    if let Some(Val::List(vs)) = doc.get("global_vars") {
        collect_vars(vs, &mut global, &mut warn);
    }
    let Some(Val::List(matches)) = doc.get("matches") else {
        warn.push("no `matches:` list found".into());
        return (out, warn);
    };
    for m in matches {
        let triggers: Vec<String> = match (m.get("trigger"), m.get("triggers")) {
            (Some(Val::Str(t)), _) => vec![t.clone()],
            (_, Some(Val::List(ts))) => ts
                .iter()
                .filter_map(|t| t.str().map(String::from))
                .collect(),
            _ => {
                let what = if m.get("regex").is_some() {
                    "regex trigger"
                } else {
                    "match without trigger"
                };
                warn.push(format!("skipped a {what}"));
                continue;
            }
        };
        let label = triggers.join(", ");
        let Some(rep) = m.get("replace").and_then(Val::str) else {
            let kind = ["form", "image_path", "html", "markdown"]
                .iter()
                .find(|k| m.get(k).is_some())
                .copied()
                .unwrap_or("replacement");
            warn.push(format!("{label}: {kind} matches are not supported"));
            continue;
        };
        let mut vars = global.clone();
        if let Some(Val::List(vs)) = m.get("vars") {
            collect_vars(vs, &mut vars, &mut warn);
        }
        let replacement = convert_replace(rep, &vars, &mut warn, &label);
        let word = m.get("word").is_some_and(Val::truthy);
        let propagate = m.get("propagate_case").is_some_and(Val::truthy);
        for t in triggers {
            if t.is_empty() {
                continue;
            }
            let mut s = Snippet::new(&t, &replacement);
            s.word_boundary = word;
            s.case_sensitive = !propagate;
            if let Some(l) = m.get("label").and_then(Val::str) {
                s.label = l.to_string();
            }
            out.push(s);
        }
    }
    (out, warn)
}

fn collect_vars(vs: &[Val], vars: &mut HashMap<String, String>, warn: &mut Vec<String>) {
    for v in vs {
        let (Some(name), Some(ty)) = (
            v.get("name").and_then(Val::str),
            v.get("type").and_then(Val::str),
        ) else {
            continue;
        };
        let value = match ty {
            "date" => {
                let f = v
                    .get("params")
                    .and_then(|p| p.get("format"))
                    .and_then(Val::str)
                    .unwrap_or("%Y-%m-%d");
                Some(format!("{{date:{f}}}"))
            }
            "clipboard" => Some("{clipboard}".into()),
            "echo" => v
                .get("params")
                .and_then(|p| p.get("echo"))
                .and_then(Val::str)
                .map(escape_braces),
            "random" => v
                .get("params")
                .and_then(|p| p.get("choices"))
                .and_then(|c| match c {
                    Val::List(l) => l.first().and_then(Val::str).map(escape_braces),
                    _ => None,
                })
                .inspect(|_| {
                    warn.push(format!(
                        "{name}: random choice imported as its first choice"
                    ))
                }),
            other => {
                warn.push(format!("{name}: variable type `{other}` is not supported"));
                None
            }
        };
        if let Some(v) = value {
            vars.insert(name.to_string(), v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"# espanso match file
matches:
  # Simple text replacement
  - trigger: ":espanso"
    replace: "Hi there!"

  - trigger: ':quote'
    replace: 'it''s {braces}'

  - trigger: ":br"
    replace: "Best Regards,\nJon Snow"

  - trigger: ":para"
    replace: |
      line one
        indented
      line three

  - trigger: ":fold"
    replace: >-
      folded
      text

  - triggers: [":hello", ":hi"]
    replace: "world"
    word: true

  - trigger: ":now"
    replace: "It's {{mytime}} ({{clip}})"
    vars:
      - name: mytime
        type: date
        params:
          format: "%H:%M"
      - name: clip
        type: clipboard

  - trigger: ":div"
    replace: "<div>$|$</div>"
    propagate_case: true

  - trigger: ":sh"
    replace: "{{output}}"
    vars:
      - name: output
        type: shell
        params:
          cmd: "echo hi"

  - regex: ":hi(?P<person>.*)\\."
    replace: "Hi {{person}}!"

  - trigger: ":form"
    form: "Hey [[name]]"

  - trigger: ":cat"
    image_path: "/cat.png"
"#;

    #[test]
    fn imports_the_simple_subset() {
        let (s, w) = import(BASE);
        let get = |t: &str| {
            s.iter()
                .find(|x| x.trigger == t)
                .unwrap_or_else(|| panic!("{t}: {s:#?}"))
        };
        assert_eq!(get(":espanso").replacement, "Hi there!");
        assert_eq!(get(":quote").replacement, "it's {{braces}}");
        assert_eq!(get(":br").replacement, "Best Regards,\nJon Snow");
        assert_eq!(
            get(":para").replacement,
            "line one\n  indented\nline three\n"
        );
        assert_eq!(get(":fold").replacement, "folded text");
        assert!(get(":hello").word_boundary && get(":hi").word_boundary);
        assert_eq!(get(":hi").replacement, "world");
        assert_eq!(get(":now").replacement, "It's {date:%H:%M} ({clipboard})");
        let div = get(":div");
        assert_eq!(div.replacement, "<div>{cursor}</div>");
        assert!(!div.case_sensitive);
        assert!(get(":espanso").case_sensitive);
        // unsupported: shell var kept as text, regex/form/image skipped
        assert_eq!(get(":sh").replacement, "{{{{output}}}}");
        assert!(s
            .iter()
            .all(|x| x.trigger != ":form" && x.trigger != ":cat"));
        assert_eq!(s.len(), 10, "{s:#?}");
        assert!(w.iter().any(|x| x.contains("shell")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("regex")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("form")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("image_path")), "{w:?}");
        // the imported escapes expand back to the literal text
        let env = crate::vars::tests::FakeEnv::new();
        assert_eq!(
            crate::vars::expand(&get(":quote").replacement, &env).text,
            "it's {braces}"
        );
        assert_eq!(
            crate::vars::expand(&get(":sh").replacement, &env).text,
            "{{output}}"
        );
        assert_eq!(
            crate::vars::expand(&get(":now").replacement, &env).text,
            "It's 05:06 (CLIP)"
        );
    }

    #[test]
    fn global_vars_and_garbage() {
        let y = "global_vars:\n  - name: today\n    type: date\n    params:\n      format: \"%d/%m\"\nmatches:\n  - trigger: \";t\"\n    replace: \"{{today}}\"\n";
        let (s, w) = import(y);
        assert_eq!(s[0].replacement, "{date:%d/%m}", "{w:?}");
        let (s, w) = import("not: [yaml");
        assert!(s.is_empty());
        assert!(!w.is_empty());
        let (s, _) = import("");
        assert!(s.is_empty());
    }
}
