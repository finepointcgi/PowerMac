//! Character diff between an original and a new name, for highlighting the
//! changed parts in the preview.

/// A run of text and whether it differs from the other name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub changed: bool,
}

fn push(v: &mut Vec<Span>, c: char, changed: bool) {
    match v.last_mut() {
        Some(s) if s.changed == changed => s.text.push(c),
        _ => v.push(Span {
            text: c.to_string(),
            changed,
        }),
    }
}

/// Spans of `old` (changed = removed) and of `new` (changed = inserted),
/// from a longest common subsequence of characters. Long names fall back to
/// common prefix/suffix.
pub fn diff(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut ra = Vec::new();
    let mut rb = Vec::new();
    for &c in &a[..pre] {
        push(&mut ra, c, false);
        push(&mut rb, c, false);
    }
    if am.len() * bm.len() <= 512 * 512 {
        // LCS table on the middle part.
        let (n, m) = (am.len(), bm.len());
        let mut t = vec![0u16; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                t[i * (m + 1) + j] = if am[i] == bm[j] {
                    t[(i + 1) * (m + 1) + j + 1] + 1
                } else {
                    t[(i + 1) * (m + 1) + j].max(t[i * (m + 1) + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                push(&mut ra, am[i], false);
                push(&mut rb, bm[j], false);
                i += 1;
                j += 1;
            } else if j < m && (i == n || t[i * (m + 1) + j + 1] >= t[(i + 1) * (m + 1) + j]) {
                push(&mut rb, bm[j], true);
                j += 1;
            } else {
                push(&mut ra, am[i], true);
                i += 1;
            }
        }
    } else {
        for &c in am {
            push(&mut ra, c, true);
        }
        for &c in bm {
            push(&mut rb, c, true);
        }
    }
    for &c in &a[a.len() - suf..] {
        push(&mut ra, c, false);
    }
    for &c in &b[b.len() - suf..] {
        push(&mut rb, c, false);
    }
    (cleanup(ra), cleanup(rb))
}

/// Fold unchanged islands of one or two characters between changes into
/// the change ("2026-1|0|-26" reads better as one block).
fn cleanup(v: Vec<Span>) -> Vec<Span> {
    let n = v.len();
    let mut out: Vec<Span> = Vec::with_capacity(n);
    for (i, mut s) in v.into_iter().enumerate() {
        if !s.changed && i > 0 && i + 1 < n && s.text.chars().count() <= 2 {
            s.changed = true;
        }
        match out.last_mut() {
            Some(l) if l.changed == s.changed => l.text.push_str(&s.text),
            _ => out.push(s),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(v: &[Span]) -> String {
        v.iter()
            .map(|s| {
                if s.changed {
                    format!("[{}]", s.text)
                } else {
                    s.text.clone()
                }
            })
            .collect()
    }

    #[test]
    fn spans() {
        let (a, b) = diff("IMG_001_IMG.jpg", "Photo_001_Photo.jpg");
        assert_eq!(show(&a), "[IMG]_001_[IMG].jpg");
        assert_eq!(show(&b), "[Photo]_001_[Photo].jpg");
        let (a, b) = diff("same", "same");
        assert_eq!((show(&a), show(&b)), ("same".into(), "same".into()));
        let (a, b) = diff("abc", "aXc");
        assert_eq!((show(&a), show(&b)), ("a[b]c".into(), "a[X]c".into()));
        let (a, b) = diff("IMG_0001.JPG", "2026-10-26 Beach 001.JPG");
        assert_eq!(show(&a), "[IMG_]0001.JPG");
        assert_eq!(show(&b), "[2026-10-26 Beach ]001.JPG");
        let (_, b) = diff("", "new");
        assert_eq!(show(&b), "[new]");
    }
}
