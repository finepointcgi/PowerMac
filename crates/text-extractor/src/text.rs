//! Turning OCR fragments (text + bounding box) into clipboard text: fragments
//! on the same row are joined left to right, rows top to bottom with line
//! breaks; optionally everything collapsed to a single line (PowerToys'
//! `OcrTextFormatter.CollapseToSingleLine`).

/// A recognized piece of text. Coordinates are normalized to the image
/// (0..=1) with the origin at the **top-left**.
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Fragment {
    pub fn new(text: impl Into<String>, x: f64, y: f64, w: f64, h: f64) -> Self {
        Self {
            text: text.into(),
            x,
            y,
            w,
            h,
        }
    }
    fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | 0x3040..=0x309F | 0x30A0..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FA1F)
}

/// Join two pieces of one row: no space between CJK characters or before
/// closing punctuation.
fn join_into(out: &mut String, next: &str) {
    let (Some(a), Some(b)) = (out.chars().last(), next.chars().next()) else {
        out.push_str(next);
        return;
    };
    let tight = (is_cjk(a) && is_cjk(b))
        || matches!(b, ',' | '.' | ':' | ';' | '!' | '?' | '%' | ')' | ']' | '}');
    if !tight && !a.is_whitespace() && !b.is_whitespace() {
        out.push(' ');
    }
    out.push_str(next);
}

/// Group fragments into rows (by vertical overlap) and build the text.
pub fn assemble(mut frags: Vec<Fragment>, single_line: bool) -> String {
    frags.retain(|f| !f.text.trim().is_empty());
    frags.sort_by(|a, b| (a.y + a.h / 2.0).total_cmp(&(b.y + b.h / 2.0)));
    let mut rows: Vec<Vec<Fragment>> = Vec::new();
    for f in frags {
        let center = f.y + f.h / 2.0;
        let row = rows.iter_mut().rev().take(2).find(|row| {
            let top = row.iter().map(|r| r.y).fold(f64::MAX, f64::min);
            let bottom = row.iter().map(|r| r.bottom()).fold(f64::MIN, f64::max);
            let row_center = (top + bottom) / 2.0;
            let h = (bottom - top).min(f.h);
            // Same row: the centers are within half a line height and the
            // boxes overlap vertically.
            (center - row_center).abs() < h * 0.5 && f.y < bottom && f.bottom() > top
        });
        match row {
            Some(r) => r.push(f),
            None => rows.push(vec![f]),
        }
    }
    let lines: Vec<String> = rows
        .into_iter()
        .map(|mut r| {
            r.sort_by(|a, b| a.x.total_cmp(&b.x));
            let mut s = String::new();
            for f in r {
                join_into(&mut s, f.text.trim());
            }
            s
        })
        .collect();
    let text = lines.join("\n");
    if single_line {
        collapse_to_single_line(&text)
    } else {
        text.trim().to_string()
    }
}

/// Line breaks become spaces; runs of spaces collapse to one.
pub fn collapse_to_single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split(['\r', '\n', ' ']).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Words for the confirmation ("Copied 42 words"); CJK characters count one each.
pub fn word_count(text: &str) -> usize {
    text.split_whitespace()
        .map(|w| {
            let cjk = w.chars().filter(|c| is_cjk(*c)).count();
            if cjk > 0 {
                cjk + usize::from(w.chars().any(|c| !is_cjk(c) && c.is_alphanumeric()))
            } else {
                1
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_and_columns() {
        // Two columns of one row each, plus a second row; given out of order.
        let frags = vec![
            Fragment::new("world", 0.5, 0.10, 0.2, 0.05),
            Fragment::new("Second line", 0.1, 0.30, 0.5, 0.05),
            Fragment::new("Hello", 0.1, 0.11, 0.2, 0.05),
        ];
        assert_eq!(assemble(frags.clone(), false), "Hello world\nSecond line");
        assert_eq!(assemble(frags, true), "Hello world Second line");
    }

    #[test]
    fn punctuation_and_cjk() {
        let frags = vec![
            Fragment::new("Total", 0.0, 0.0, 0.1, 0.05),
            Fragment::new(":", 0.11, 0.0, 0.01, 0.05),
            Fragment::new("42%", 0.13, 0.0, 0.05, 0.05),
            Fragment::new("日本", 0.0, 0.2, 0.1, 0.05),
            Fragment::new("語", 0.11, 0.2, 0.05, 0.05),
        ];
        assert_eq!(assemble(frags, false), "Total: 42%\n日本語");
    }

    #[test]
    fn blank_fragments_and_collapse() {
        assert_eq!(
            assemble(vec![Fragment::new("  ", 0.0, 0.0, 1.0, 1.0)], false),
            ""
        );
        assert_eq!(collapse_to_single_line("a\r\n b  \n\nc "), "a b c");
        assert_eq!(word_count("Copied forty two words"), 4);
        assert_eq!(word_count("日本語 text"), 4);
        assert_eq!(word_count(""), 0);
    }
}
