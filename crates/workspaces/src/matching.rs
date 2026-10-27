//! Which running app / window belongs to which workspace entry.
//!
//! Apps match by bundle id (macOS), else by path, else by name — PowerToys
//! matches `name == name || path == path`. Among several candidate windows
//! the nearest one wins, with PowerToys' penalties: both minimized = 0,
//! both normal = 1 + edge distance, one minimized = 10000 + edge distance.
//! A window showing another document than the saved one is never reused.

use crate::backend::{AppIdentity, LiveWindow};
use crate::geometry::Rect;
use crate::model::{App, WindowState};

pub const PENALTY_BOTH_NORMAL: i64 = 1;
pub const PENALTY_MINIMIZED_MISMATCH: i64 = 10_000;
/// The saved app has a document, the window has none (or it is unknown).
pub const PENALTY_UNKNOWN_DOCUMENT: i64 = 5_000;

fn norm_path(p: &str) -> String {
    let p = p.trim().trim_end_matches(['/', '\\']);
    if cfg!(windows) || p.contains('\\') {
        p.replace('/', "\\").to_lowercase()
    } else {
        p.to_string()
    }
}

/// A key identifying the app of an entry (for grouping).
pub fn app_key(a: &App) -> String {
    if !a.bundle_id.is_empty() {
        format!("b:{}", a.bundle_id.to_lowercase())
    } else if !a.path.is_empty() {
        format!("p:{}", norm_path(&a.path))
    } else {
        format!("n:{}", a.name.to_lowercase())
    }
}

/// Whether a running app is the saved one.
pub fn same_app(a: &App, live: &AppIdentity) -> bool {
    if !a.bundle_id.is_empty() && !live.bundle_id.is_empty() {
        return a.bundle_id.eq_ignore_ascii_case(&live.bundle_id);
    }
    if !a.path.is_empty() && !live.path.is_empty() && norm_path(&a.path) == norm_path(&live.path) {
        return true;
    }
    !a.name.is_empty() && a.name.eq_ignore_ascii_case(&live.name)
}

fn same_document(a: &str, b: &str) -> bool {
    norm_path(a) == norm_path(b)
}

/// PowerToys-style distance between a saved entry (at its mapped `target`
/// frame) and a live window; `None` if the window can't stand in for it.
pub fn distance(app: &App, target: &Rect, w: &LiveWindow) -> Option<i64> {
    if !same_app(app, &w.app) {
        return None;
    }
    let mut extra = 0;
    if let Some(doc) = &app.document {
        match &w.document {
            Some(d) if same_document(doc, d) => {}
            Some(_) => return None,
            None => extra = PENALTY_UNKNOWN_DOCUMENT,
        }
    }
    let a_min = app.state == WindowState::Minimized;
    let w_min = w.state == WindowState::Minimized;
    if a_min && w_min {
        return Some(extra);
    }
    let penalty = if a_min != w_min { PENALTY_MINIMIZED_MISMATCH } else { PENALTY_BOTH_NORMAL };
    Some(extra + penalty + target.edge_distance(&w.frame))
}

/// Greedy nearest assignment of existing windows to entries (PowerToys'
/// "move existing windows" phase): repeatedly take the globally closest
/// (entry, window) pair. `entries` are (index, app, target frame).
/// Returns (entry index, window index) pairs.
pub fn assign_existing(entries: &[(usize, &App, Rect)], windows: &[LiveWindow]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut used_e = vec![false; entries.len()];
    let mut used_w = vec![false; windows.len()];
    loop {
        let mut best: Option<(i64, usize, usize)> = None;
        for (ei, (_, app, target)) in entries.iter().enumerate() {
            if used_e[ei] {
                continue;
            }
            for (wi, w) in windows.iter().enumerate() {
                if used_w[wi] {
                    continue;
                }
                if let Some(d) = distance(app, target, w) {
                    if best.is_none_or(|(bd, _, _)| d < bd) {
                        best = Some((d, ei, wi));
                    }
                }
            }
        }
        let Some((_, ei, wi)) = best else { break };
        used_e[ei] = true;
        used_w[wi] = true;
        out.push((entries[ei].0, wi));
    }
    out
}

/// Lower-case alphanumerics only ("Visual Studio Code" → "visualstudiocode").
pub fn normalize_name(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_suffix(".exe").or_else(|| s.strip_suffix(".EXE")).unwrap_or(s);
    let s = s.strip_suffix(".app").unwrap_or(s);
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// Windows app names (PowerToys `application` or exe stem) with a
/// different name on macOS.
const ALIASES: &[(&str, &[&str])] = &[
    ("notepad", &["TextEdit"]),
    ("notepad++", &["TextEdit"]),
    ("wordpad", &["TextEdit"]),
    ("fileexplorer", &["Finder"]),
    ("explorer", &["Finder"]),
    ("windowsexplorer", &["Finder"]),
    ("windowsterminal", &["Terminal", "iTerm"]),
    ("terminal", &["Terminal", "iTerm"]),
    ("wt", &["Terminal", "iTerm"]),
    ("cmd", &["Terminal"]),
    ("commandprompt", &["Terminal"]),
    ("powershell", &["Terminal"]),
    ("windowspowershell", &["Terminal"]),
    ("pwsh", &["Terminal"]),
    ("calc", &["Calculator"]),
    ("calculator", &["Calculator"]),
    ("msedge", &["Microsoft Edge"]),
    ("chrome", &["Google Chrome"]),
    ("firefox", &["Firefox"]),
    ("code", &["Visual Studio Code"]),
    ("winword", &["Microsoft Word"]),
    ("excel", &["Microsoft Excel"]),
    ("powerpnt", &["Microsoft PowerPoint"]),
    ("outlook", &["Microsoft Outlook"]),
    ("olk", &["Microsoft Outlook"]),
    ("onenote", &["Microsoft OneNote"]),
    ("teams", &["Microsoft Teams"]),
    ("msteams", &["Microsoft Teams"]),
    ("mspaint", &["Preview"]),
    ("paint", &["Preview"]),
    ("photos", &["Photos", "Preview"]),
    ("snippingtool", &["Screenshot"]),
    ("taskmgr", &["Activity Monitor"]),
    ("taskmanager", &["Activity Monitor"]),
    ("settings", &["System Settings"]),
    ("systemsettings", &["System Settings"]),
    ("mail", &["Mail"]),
    ("calendar", &["Calendar"]),
    ("spotify", &["Spotify"]),
    ("slack", &["Slack"]),
    ("discord", &["Discord"]),
    ("zoom", &["zoom.us"]),
    ("obsidian", &["Obsidian"]),
    ("notion", &["Notion"]),
    ("figma", &["Figma"]),
    ("devenv", &["Visual Studio Code"]),
];

/// An installed app (for mapping imported Windows entries).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InstalledApp {
    pub name: String,
    pub bundle_id: String,
    pub path: String,
}

/// The installed app a Windows entry corresponds to: same name (ignoring
/// case, spaces and punctuation; the exe stem works too), PowerToys' "PWA
/// name (Browser)" names map to the PWA name, then the alias table.
pub fn find_installed<'a>(win_name: &str, win_path: &str, installed: &'a [InstalledApp]) -> Option<&'a InstalledApp> {
    let by_norm = |n: &str| -> Option<&'a InstalledApp> {
        let n = normalize_name(n);
        if n.is_empty() {
            return None;
        }
        installed.iter().find(|a| normalize_name(&a.name) == n)
    };
    let stem = win_path.rsplit(['\\', '/']).next().unwrap_or("");
    let mut candidates: Vec<String> = vec![win_name.to_string()];
    // "Outlook (PWA) (Microsoft Edge)" → "Outlook (PWA)", "Outlook"
    let mut n = win_name.trim().to_string();
    while let Some(i) = n.rfind(" (") {
        n.truncate(i);
        candidates.push(n.clone());
    }
    candidates.push(stem.to_string());
    for c in &candidates {
        if let Some(a) = by_norm(c) {
            return Some(a);
        }
    }
    for c in &candidates {
        let key = normalize_name(c);
        if let Some((_, targets)) = ALIASES.iter().find(|(k, _)| *k == key) {
            for t in *targets {
                if let Some(a) = by_norm(t) {
                    return Some(a);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(id: u64, bundle: &str, frame: Rect, state: WindowState) -> LiveWindow {
        LiveWindow {
            id,
            pid: 10,
            app: AppIdentity { bundle_id: bundle.into(), path: format!("/Applications/{bundle}.app"), name: bundle.into() },
            frame,
            state,
            ..Default::default()
        }
    }

    fn app(bundle: &str, frame: Rect) -> App {
        App { name: bundle.into(), bundle_id: bundle.into(), frame, ..Default::default() }
    }

    #[test]
    fn identity_rules() {
        let a = App { bundle_id: "com.apple.TextEdit".into(), path: "/System/Applications/TextEdit.app".into(), name: "TextEdit".into(), ..Default::default() };
        let live = AppIdentity { bundle_id: "com.apple.textedit".into(), path: "/elsewhere/TextEdit.app".into(), name: "TextEdit".into() };
        assert!(same_app(&a, &live));
        let other = AppIdentity { bundle_id: "com.other".into(), name: "TextEdit".into(), ..Default::default() };
        // bundle ids differ: the name doesn't rescue it
        assert!(!same_app(&a, &other));
        // Windows: paths compare case-insensitively
        let w = App { path: r"C:\Program Files\App\App.exe".into(), name: "App".into(), ..Default::default() };
        assert!(same_app(&w, &AppIdentity { path: r"c:\program files\app\app.exe".into(), ..Default::default() }));
        // name fallback
        assert!(same_app(&w, &AppIdentity { name: "app".into(), path: r"D:\x.exe".into(), ..Default::default() }));
    }

    #[test]
    fn nearest_window_wins() {
        let a1 = app("ed", Rect::new(0, 0, 500, 500));
        let a2 = app("ed", Rect::new(1000, 0, 500, 500));
        let ws = vec![
            win(1, "ed", Rect::new(990, 10, 500, 500), WindowState::Normal),
            win(2, "ed", Rect::new(20, 0, 500, 500), WindowState::Normal),
            win(3, "other", Rect::new(0, 0, 500, 500), WindowState::Normal),
        ];
        let entries = vec![(0, &a1, a1.frame), (1, &a2, a2.frame)];
        let mut got = assign_existing(&entries, &ws);
        got.sort();
        assert_eq!(got, vec![(0, 1), (1, 0)]);
    }

    #[test]
    fn minimized_pairs_first() {
        let mut a = app("ed", Rect::new(0, 0, 500, 500));
        a.state = WindowState::Minimized;
        let normal = win(1, "ed", Rect::new(0, 0, 500, 500), WindowState::Normal);
        let mini = win(2, "ed", Rect::new(3000, 3000, 10, 10), WindowState::Minimized);
        assert_eq!(distance(&a, &a.frame, &mini), Some(0));
        assert_eq!(distance(&a, &a.frame, &normal), Some(PENALTY_MINIMIZED_MISMATCH));
        let entries = vec![(0, &a, a.frame)];
        assert_eq!(assign_existing(&entries, &[normal, mini]), vec![(0, 1)]);
    }

    #[test]
    fn documents_must_agree() {
        let mut a = app("ed", Rect::new(0, 0, 500, 500));
        a.document = Some("/tmp/a.txt".into());
        let mut w = win(1, "ed", a.frame, WindowState::Normal);
        w.document = Some("/tmp/b.txt".into());
        assert_eq!(distance(&a, &a.frame, &w), None);
        w.document = Some("/tmp/a.txt".into());
        assert_eq!(distance(&a, &a.frame, &w), Some(1));
        w.document = None;
        assert_eq!(distance(&a, &a.frame, &w), Some(PENALTY_UNKNOWN_DOCUMENT + 1));
    }

    #[test]
    fn installed_lookup() {
        let inst = vec![
            InstalledApp { name: "TextEdit".into(), bundle_id: "com.apple.TextEdit".into(), path: "/System/Applications/TextEdit.app".into() },
            InstalledApp { name: "Visual Studio Code".into(), bundle_id: "com.microsoft.VSCode".into(), path: "/Applications/Visual Studio Code.app".into() },
            InstalledApp { name: "Microsoft Edge".into(), bundle_id: "com.microsoft.edgemac".into(), path: "/Applications/Microsoft Edge.app".into() },
            InstalledApp { name: "Spotify".into(), bundle_id: "com.spotify.client".into(), path: "/Applications/Spotify.app".into() },
        ];
        let f = |n: &str, p: &str| find_installed(n, p, &inst).map(|a| a.name.as_str());
        assert_eq!(f("Notepad", r"C:\Windows\System32\notepad.exe"), Some("TextEdit"));
        assert_eq!(f("Visual Studio Code", r"C:\Users\me\AppData\Local\Programs\Microsoft VS Code\Code.exe"), Some("Visual Studio Code"));
        assert_eq!(f("", r"C:\x\Code.exe"), Some("Visual Studio Code"));
        assert_eq!(f("Spotify (Microsoft Edge)", r"C:\x\msedge.exe"), Some("Spotify"));
        assert_eq!(f("Some PWA (Microsoft Edge)", r"C:\x\msedge.exe"), Some("Microsoft Edge"));
        assert_eq!(f("Paint.NET", r"C:\x\paintdotnet.exe"), None);
    }

    #[test]
    fn names_normalize() {
        assert_eq!(normalize_name("Visual Studio Code"), "visualstudiocode");
        assert_eq!(normalize_name("notepad.exe"), "notepad");
        assert_eq!(normalize_name("zoom.us"), "zoomus");
    }
}
