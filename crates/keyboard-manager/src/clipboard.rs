//! Clipboard helpers for "paste as plain text".
//!
//! Text goes through [`arboard`]. Snapshots used to restore the original
//! (rich) clipboard are full-fidelity on macOS (every type of every
//! `NSPasteboardItem`); elsewhere they keep what arboard can round-trip
//! (text, HTML and image).

use anyhow::{anyhow, Result};

/// Read the clipboard as plain text: the text flavor if present, otherwise
/// the HTML flavor converted to text (as PowerToys Advanced Paste does).
/// `Ok(None)` when the clipboard holds no text at all.
pub fn read_plain_text(cb: &mut arboard::Clipboard) -> Result<Option<String>> {
    match cb.get_text() {
        Ok(t) if !t.is_empty() => return Ok(Some(t)),
        Ok(_) | Err(arboard::Error::ContentNotAvailable) => {}
        Err(e) => return Err(anyhow!("reading clipboard text: {e}")),
    }
    match cb.get().html() {
        Ok(h) if !h.trim().is_empty() => {
            let t = html_to_text(&h);
            Ok((!t.is_empty()).then_some(t))
        }
        _ => Ok(None),
    }
}

/// Replace the clipboard with its plain-text form (dropping RTF/HTML/images).
/// Returns the text, or `None` (clipboard untouched) when there is no text.
pub fn convert_to_plain_text() -> Result<Option<String>> {
    let mut cb = arboard::Clipboard::new().map_err(|e| anyhow!("opening clipboard: {e}"))?;
    let Some(text) = read_plain_text(&mut cb)? else {
        return Ok(None);
    };
    cb.set_text(text.clone())
        .map_err(|e| anyhow!("writing clipboard: {e}"))?;
    Ok(Some(text))
}

/// A token that changes whenever the clipboard contents change
/// (`NSPasteboard.changeCount`, `GetClipboardSequenceNumber`).
pub fn change_count() -> Option<i64> {
    #[cfg(target_os = "macos")]
    {
        Some(mac::change_count())
    }
    #[cfg(windows)]
    {
        Some(
            unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
                as i64,
        )
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

/// A copy of the clipboard contents that can be put back later.
pub struct Snapshot {
    #[cfg(target_os = "macos")]
    items: Vec<Vec<(String, Vec<u8>)>>,
    #[cfg(not(target_os = "macos"))]
    text: Option<String>,
    #[cfg(not(target_os = "macos"))]
    html: Option<String>,
    #[cfg(not(target_os = "macos"))]
    image: Option<arboard::ImageData<'static>>,
}

impl Snapshot {
    pub fn capture() -> Result<Snapshot> {
        #[cfg(target_os = "macos")]
        {
            Ok(Snapshot {
                items: mac::capture(),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut cb =
                arboard::Clipboard::new().map_err(|e| anyhow!("opening clipboard: {e}"))?;
            Ok(Snapshot {
                text: cb.get_text().ok(),
                html: cb.get().html().ok(),
                image: cb.get_image().ok(),
            })
        }
    }

    /// Whether the snapshot holds nothing.
    pub fn is_empty(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.items.iter().all(|i| i.is_empty())
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.text.is_none() && self.html.is_none() && self.image.is_none()
        }
    }

    /// Number of distinct data flavors captured.
    pub fn flavor_count(&self) -> usize {
        #[cfg(target_os = "macos")]
        {
            self.items.iter().map(Vec::len).sum()
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.text.is_some() as usize
                + self.html.is_some() as usize
                + self.image.is_some() as usize
        }
    }

    pub fn restore(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            mac::restore(&self.items)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut cb =
                arboard::Clipboard::new().map_err(|e| anyhow!("opening clipboard: {e}"))?;
            let r = if let Some(h) = &self.html {
                cb.set_html(h.clone(), self.text.clone())
            } else if let Some(i) = &self.image {
                cb.set_image(i.clone())
            } else if let Some(t) = &self.text {
                cb.set_text(t.clone())
            } else {
                cb.clear()
            };
            r.map_err(|e| anyhow!("restoring clipboard: {e}"))
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use anyhow::{bail, Result};
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSData, NSString};

    pub fn change_count() -> i64 {
        autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount() as i64)
    }

    pub fn capture() -> Vec<Vec<(String, Vec<u8>)>> {
        autoreleasepool(|_| {
            let pb = NSPasteboard::generalPasteboard();
            let mut out = Vec::new();
            if let Some(items) = pb.pasteboardItems() {
                for item in items.iter() {
                    let mut flavors = Vec::new();
                    for ty in item.types().iter() {
                        if let Some(data) = item.dataForType(&ty) {
                            flavors.push((ty.to_string(), data.to_vec()));
                        }
                    }
                    out.push(flavors);
                }
            }
            out
        })
    }

    pub fn restore(items: &[Vec<(String, Vec<u8>)>]) -> Result<()> {
        autoreleasepool(|_| {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            let objs: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
                .iter()
                .filter(|i| !i.is_empty())
                .map(|flavors| {
                    let item = NSPasteboardItem::new();
                    for (ty, bytes) in flavors {
                        item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(ty));
                    }
                    ProtocolObject::from_retained(item)
                })
                .collect();
            if objs.is_empty() {
                return Ok(());
            }
            if !pb.writeObjects(&NSArray::from_retained_slice(&objs)) {
                bail!("NSPasteboard writeObjects failed");
            }
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// HTML -> text
// ---------------------------------------------------------------------------

const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tr",
    "ul",
];
const SKIP: &[&str] = &["head", "script", "style", "template", "title"];

fn decode_entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let n = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse().ok()?
        };
        return char::from_u32(n);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "hellip" => '\u{2026}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "copy" => '\u{a9}',
        "reg" => '\u{ae}',
        "trade" => '\u{2122}',
        "bull" => '\u{2022}',
        "middot" => '\u{b7}',
        "euro" => '\u{20ac}',
        _ => return None,
    })
}

fn line_break(out: &mut String) {
    while out.ends_with([' ', '\t']) {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Convert an HTML fragment (or a Windows `CF_HTML` payload) to plain text,
/// keeping paragraph / line / table-cell boundaries and `<pre>` whitespace.
pub fn html_to_text(html: &str) -> String {
    // CF_HTML: only the fragment between the markers is the copied content.
    let html = match (
        html.find("<!--StartFragment-->"),
        html.find("<!--EndFragment-->"),
    ) {
        (Some(s), Some(e)) if e > s => &html[s + "<!--StartFragment-->".len()..e],
        _ => html,
    };
    let mut out = String::new();
    let mut pending_space = false;
    let mut pre = 0usize;
    let mut skip: Option<String> = None;
    let mut rest = html;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |i| &after[i + 3..]);
            continue;
        }
        if rest.starts_with('<') {
            let (tag, after) = match rest.find('>') {
                Some(end) => (&rest[1..end], &rest[end + 1..]),
                None => (&rest[1..], ""),
            };
            rest = after;
            let closing = tag.starts_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if let Some(s) = &skip {
                if closing && *s == name {
                    skip = None;
                }
                continue;
            }
            if name.is_empty() {
                continue;
            }
            if !closing && SKIP.contains(&name.as_str()) && !tag.ends_with('/') {
                skip = Some(name);
                continue;
            }
            match name.as_str() {
                "br" => {
                    out.push('\n');
                    pending_space = false;
                }
                "td" | "th" if closing => {
                    out.push('\t');
                    pending_space = false;
                }
                "tr" if closing => {
                    while out.ends_with('\t') {
                        out.pop();
                    }
                    line_break(&mut out);
                    pending_space = false;
                }
                n if BLOCK.contains(&n) => {
                    if n == "pre" {
                        pre = if closing {
                            pre.saturating_sub(1)
                        } else {
                            pre + 1
                        };
                    }
                    line_break(&mut out);
                    pending_space = false;
                }
                _ => {}
            }
            continue;
        }
        // text run up to the next tag
        let end = rest.find('<').unwrap_or(rest.len());
        let text = &rest[..end];
        rest = &rest[end..];
        if skip.is_some() {
            continue;
        }
        let mut chars = text.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            let c = if c == '&' {
                let tail = &text[i + 1..];
                match tail
                    .find(';')
                    .filter(|&j| j <= 10)
                    .and_then(|j| decode_entity(&tail[..j]).map(|ch| (j, ch)))
                {
                    Some((j, ch)) => {
                        for _ in 0..=j {
                            chars.next();
                        }
                        ch
                    }
                    None => '&',
                }
            } else {
                c
            };
            if pre == 0 && matches!(c, ' ' | '\t' | '\r' | '\n' | '\x0c') {
                pending_space = true;
                continue;
            }
            if pending_space && !out.is_empty() && !out.ends_with(['\n', '\t', ' ']) {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        }
    }
    out.trim_end().trim_start_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_basic() {
        assert_eq!(html_to_text("<b>Hello</b>   <i>world</i>"), "Hello world");
        assert_eq!(html_to_text("<p>One</p><p>Two</p>"), "One\nTwo");
        assert_eq!(html_to_text("a<br>b<br/>c"), "a\nb\nc");
        assert_eq!(
            html_to_text("Fish &amp; chips &lt;3 &#x263A; &#65; &bogus; &nbsp;x"),
            "Fish & chips <3 \u{263a} A &bogus; \u{a0}x"
        );
        assert_eq!(
            html_to_text("<html><head><style>p{color:red}</style><title>T</title></head><body><script>x()</script><p>Body</p></body></html>"),
            "Body"
        );
        assert_eq!(html_to_text("<ul><li>a</li><li>b</li></ul>"), "a\nb");
        assert_eq!(html_to_text("<pre>  x\n  y</pre>"), "  x\n  y");
        assert_eq!(
            html_to_text(
                "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>"
            ),
            "a\tb\nc\td"
        );
        assert_eq!(html_to_text("x <!-- comment --> y"), "x y");
    }

    #[test]
    fn html_cf_html_fragment() {
        let cf = "Version:0.9\r\nStartHTML:00000097\r\nEndHTML:00000170\r\nStartFragment:00000131\r\nEndFragment:00000134\r\n<html><body>\r\n<!--StartFragment--><span style=\"font-weight:bold\">Bold</span> text<!--EndFragment-->\r\n</body>\r\n</html>";
        assert_eq!(html_to_text(cf), "Bold text");
    }

    /// Touches the real system clipboard: run with `cargo test -- --ignored`.
    /// The original contents are restored afterwards.
    #[test]
    #[ignore]
    fn system_clipboard_plain_text_round_trip() {
        struct Restore(Snapshot);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = self.0.restore();
            }
        }
        let _original = Restore(Snapshot::capture().unwrap());
        let mut cb = arboard::Clipboard::new().unwrap();
        let html = "<p><b>Rich</b> <i>text</i> &amp; more</p>";
        cb.set_html(html, Some("Rich text & more")).unwrap();
        let before = change_count();
        let rich = Snapshot::capture().unwrap();
        assert!(rich.flavor_count() >= 2, "html + text flavors");

        let text = convert_to_plain_text().unwrap();
        assert_eq!(text.as_deref(), Some("Rich text & more"));
        assert_ne!(change_count(), before);
        let after = Snapshot::capture().unwrap();
        assert!(
            after.flavor_count() < rich.flavor_count(),
            "formatting removed"
        );
        assert!(cb.get().html().is_err(), "no HTML left");
        assert_eq!(cb.get_text().unwrap(), "Rich text & more");

        // The snapshot restores the rich flavors
        rich.restore().unwrap();
        assert!(cb.get().html().unwrap().contains("<b>Rich</b>"));

        // HTML-only clipboard falls back to converting the HTML
        cb.set_html("<div>Only <b>html</b></div><div>here</div>", None::<&str>)
            .unwrap();
        assert_eq!(
            read_plain_text(&mut cb).unwrap().as_deref(),
            Some("Only html\nhere")
        );
    }
}
