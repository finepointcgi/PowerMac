//! "Copy path" formatting.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What "Copy path" puts on the clipboard for each item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum CopyStyle {
    /// `/Users/me/Documents/a b.txt` (Windows: `C:\Users\me\a b.txt`).
    #[default]
    Posix,
    /// `~/Documents/a b.txt` (Windows: `%USERPROFILE%\a b.txt`).
    Tilde,
    /// `file:///Users/me/Documents/a%20b.txt` (folders end in `/`).
    FileUrl,
    /// `a b.txt`.
    Name,
    /// `'/Users/me/Documents/a b.txt'`, quoted for a shell.
    Quoted,
}

impl CopyStyle {
    pub const ALL: [CopyStyle; 5] = [
        CopyStyle::Posix,
        CopyStyle::Tilde,
        CopyStyle::FileUrl,
        CopyStyle::Name,
        CopyStyle::Quoted,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CopyStyle::Posix => "Path",
            CopyStyle::Tilde => "Path with ~",
            CopyStyle::FileUrl => "File URL",
            CopyStyle::Name => "Name",
            CopyStyle::Quoted => "Shell-quoted path",
        }
    }

    /// The CLI spelling (`posix`, `tilde`, `url`, `name`, `quoted`).
    pub fn cli_name(self) -> &'static str {
        match self {
            CopyStyle::Posix => "posix",
            CopyStyle::Tilde => "tilde",
            CopyStyle::FileUrl => "url",
            CopyStyle::Name => "name",
            CopyStyle::Quoted => "quoted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "posix" | "path" | "full" => CopyStyle::Posix,
            "tilde" | "~" | "home" => CopyStyle::Tilde,
            "url" | "file-url" | "fileurl" | "uri" => CopyStyle::FileUrl,
            "name" | "filename" => CopyStyle::Name,
            "quoted" | "shell" => CopyStyle::Quoted,
            _ => return None,
        })
    }
}

/// Characters kept as they are in a file URL's path (RFC 3986 unreserved
/// plus the path sub-delimiters Foundation leaves alone).
fn url_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&b)
}

/// Percent-encode a path for a `file://` URL.
pub fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if url_safe(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `'s'` for a POSIX shell (plain when it needs no quoting).
pub fn shell_quote_minimal(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./,:@%+=".contains(&b));
    if plain {
        s.to_string()
    } else {
        crate::integration::shell_quote(s)
    }
}

/// The absolute form of `p` (relative paths against the current folder),
/// without trailing separators.
fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Format one path. `home` is the home folder for [`CopyStyle::Tilde`];
/// `is_dir` adds the trailing `/` of folder URLs.
pub fn format_path_with(p: &Path, style: CopyStyle, home: Option<&Path>, is_dir: bool) -> String {
    let p = absolute(p);
    let windows = cfg!(windows);
    match style {
        CopyStyle::Posix => p.display().to_string(),
        CopyStyle::Name => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string()),
        CopyStyle::Tilde => {
            if let Some(h) = home {
                if let Ok(rest) = p.strip_prefix(h) {
                    let head = if windows { "%USERPROFILE%" } else { "~" };
                    let sep = std::path::MAIN_SEPARATOR;
                    return if rest.as_os_str().is_empty() {
                        head.to_string()
                    } else {
                        format!("{head}{sep}{}", rest.display())
                    };
                }
            }
            p.display().to_string()
        }
        CopyStyle::FileUrl => {
            let mut s = p.to_string_lossy().replace('\\', "/");
            if windows && !s.starts_with('/') {
                s.insert(0, '/');
            }
            if is_dir && !s.ends_with('/') {
                s.push('/');
            }
            format!("file://{}", percent_encode_path(&s))
        }
        CopyStyle::Quoted => {
            if windows {
                format!("\"{}\"", p.display())
            } else {
                shell_quote_minimal(&p.to_string_lossy())
            }
        }
    }
}

/// Format one path with the real home folder.
pub fn format_path(p: &Path, style: CopyStyle) -> String {
    format_path_with(p, style, dirs::home_dir().as_deref(), p.is_dir())
}

/// Format several paths, one per line.
pub fn format_paths(paths: &[PathBuf], style: CopyStyle) -> String {
    paths
        .iter()
        .map(|p| format_path(p, style))
        .collect::<Vec<_>>()
        .join("\n")
}
