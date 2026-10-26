//! Name clean-up and text formatting (PowerToys `GetTrimmedFileName` and
//! `GetTransformedFileName`).

use super::{split_ext, ApplyTo, TextCase};

/// Drop leading whitespace and trailing whitespace and dots.
pub fn trim_name(s: &str) -> String {
    s.trim_start()
        .trim_end_matches(|c: char| c.is_whitespace() || c == '.')
        .to_string()
}

fn is_punct(c: char) -> bool {
    !c.is_alphanumeric() && !c.is_whitespace()
}

const TITLE_EXCEPTIONS: &[&str] = &[
    "a", "an", "to", "the", "at", "by", "for", "in", "of", "on", "up", "and", "as", "but", "or",
    "nor",
];

fn upper(c: char) -> String {
    c.to_uppercase().collect()
}
fn lower(c: char) -> String {
    c.to_lowercase().collect()
}

/// A word starts at `i` (after whitespace, or after punctuation that isn't
/// the apostrophe of a contraction such as "don't").
fn word_start(s: &[char], i: usize) -> bool {
    if i == 0 || s[i - 1].is_whitespace() {
        return true;
    }
    is_punct(s[i - 1])
        && (s[i - 1] != '\'' || i == 1 || is_punct(s[i - 2]) || s[i - 2].is_whitespace())
}

/// Title Case ("the lord of the rings" → "The Lord of the Rings") or
/// Capitalize each word (`title == false`).
fn words(stem: &str, title: bool) -> String {
    let s: Vec<char> = stem.chars().collect();
    let mut len = s.len();
    while len > 0 && (s[len - 1].is_whitespace() || is_punct(s[len - 1])) {
        len -= 1;
    }
    let mut out = String::with_capacity(stem.len());
    let mut first = true;
    let mut i = 0;
    while i < len {
        let c = s[i];
        if word_start(&s, i) {
            if c.is_whitespace() || is_punct(c) {
                out.push(c);
                i += 1;
                continue;
            }
            if title {
                let mut wl = 0;
                while i + wl < len && !s[i + wl].is_whitespace() && !is_punct(s[i + wl]) {
                    wl += 1;
                }
                let word: String = s[i..i + wl].iter().flat_map(|c| c.to_lowercase()).collect();
                if first || i + wl == len || !TITLE_EXCEPTIONS.contains(&word.as_str()) {
                    out.push_str(&upper(c));
                    first = false;
                } else {
                    out.push_str(&lower(c));
                }
            } else {
                out.push_str(&upper(c));
            }
        } else {
            out.push_str(&lower(c));
        }
        i += 1;
    }
    out.extend(&s[len..]);
    out
}

/// Apply `case` to `name` (a full name; folders have no extension).
pub fn transform(name: &str, case: TextCase, apply_to: ApplyTo, is_dir: bool) -> String {
    let (stem, ext) = if is_dir { (name, "") } else { split_ext(name) };
    match case {
        TextCase::None => name.to_string(),
        TextCase::Upper | TextCase::Lower => {
            let f = |s: &str| {
                if case == TextCase::Upper {
                    s.to_uppercase()
                } else {
                    s.to_lowercase()
                }
            };
            match apply_to {
                _ if is_dir => f(name),
                ApplyTo::NameOnly => format!("{}{ext}", f(stem)),
                ApplyTo::ExtensionOnly if !ext.is_empty() => format!("{stem}{}", f(ext)),
                ApplyTo::ExtensionOnly => name.to_string(),
                ApplyTo::NameAndExtension => f(name),
            }
        }
        TextCase::Title | TextCase::Capitalize => {
            if apply_to == ApplyTo::ExtensionOnly && !is_dir {
                return name.to_string();
            }
            format!("{}{ext}", words(stem, case == TextCase::Title))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim() {
        assert_eq!(trim_name("  a b . . "), "a b");
        assert_eq!(trim_name("a.txt"), "a.txt");
        assert_eq!(trim_name("..."), "");
    }

    #[test]
    fn cases() {
        use ApplyTo::*;
        use TextCase::*;
        assert_eq!(
            transform("My File.Txt", Upper, NameAndExtension, false),
            "MY FILE.TXT"
        );
        assert_eq!(
            transform("My File.Txt", Upper, NameOnly, false),
            "MY FILE.Txt"
        );
        assert_eq!(
            transform("My File.Txt", Lower, ExtensionOnly, false),
            "My File.txt"
        );
        assert_eq!(transform("My.Dir", Lower, NameOnly, true), "my.dir");
        assert_eq!(
            transform("the lord OF the rings.mkv", Title, NameAndExtension, false),
            "The Lord of the Rings.mkv"
        );
        assert_eq!(
            transform("a tale of two cities - an epic.txt", Title, NameOnly, false),
            "A Tale of Two Cities - an Epic.txt"
        );
        assert_eq!(
            transform("don't stop me_now.MP3", Capitalize, NameAndExtension, false),
            "Don't Stop Me_Now.MP3"
        );
        assert_eq!(
            transform("'quoted' word", Capitalize, NameOnly, true),
            "'Quoted' Word"
        );
        assert_eq!(
            transform("hello world.txt", Title, ExtensionOnly, false),
            "hello world.txt"
        );
        assert_eq!(
            transform("straße", Upper, NameAndExtension, true),
            "STRASSE"
        );
        assert_eq!(
            transform("élan vital", Capitalize, NameOnly, true),
            "Élan Vital"
        );
        assert_eq!(
            transform("end with of.", Title, NameOnly, true),
            "End With Of."
        );
    }
}
