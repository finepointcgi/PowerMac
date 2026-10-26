//! Which windows FancyZones processes (`FancyZonesWindowProcessing` +
//! `FancyZonesWindowUtils::IsExcluded`), platform independent.

use crate::config::Settings;

/// Classification of a top-level window (`ProcessabilityType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowKind {
    /// Normal resizable window.
    #[default]
    Standard,
    /// Window with a visible owner (Windows) / dialog (macOS `AXDialog`,
    /// `AXSystemDialog`). Processed only with `allow_snap_child_windows`.
    Child,
    /// Popup without caption/thick frame (Windows `WS_POPUP`) / floating panel
    /// (macOS `AXFloatingWindow`, unknown subroles). Processed only with
    /// `allow_snap_popup_windows`.
    Popup,
    /// Never processed: minimized, invisible, tool windows, sheets, ...
    Never,
}

/// What is known about the app owning a window, for exclusion matching.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppIdentity {
    /// Executable path (Windows) / app bundle or executable path (macOS).
    pub path: Option<String>,
    /// macOS bundle identifier.
    pub bundle_id: Option<String>,
    /// Localized app name (macOS).
    pub name: Option<String>,
    /// Window title (Windows: PowerToys also matches excluded entries as
    /// title substrings).
    pub title: Option<String>,
}

impl AppIdentity {
    pub fn from_path(path: Option<String>) -> Self {
        Self { path, ..Default::default() }
    }
}

/// PowerToys `find_app_name_in_path`: `entry` occurs in `path` such that the
/// (last) occurrence covers the start of the file name — e.g. `NOTEPAD.EXE`,
/// `NOTEPAD` or `WINDOWS\NOTEPAD.EXE` all match `C:\WINDOWS\NOTEPAD.EXE`.
/// Both arguments must already be upper-case.
fn app_name_in_path(path: &str, entry: &str) -> bool {
    let Some(pos) = path.rfind(entry) else { return false };
    let last_sep = path.rfind(['\\', '/']);
    match last_sep {
        Some(s) => pos <= s + 1 && pos + entry.len() > s,
        None => pos == 0,
    }
}

/// `true` if the app matches one of the `excluded` entries (case-insensitive).
pub fn is_excluded(excluded: &[String], app: &AppIdentity) -> bool {
    let path = app.path.as_deref().map(str::to_uppercase);
    let bundle = app.bundle_id.as_deref().map(str::to_uppercase);
    let name = app.name.as_deref().map(str::to_uppercase);
    let title = app.title.as_deref().map(str::to_uppercase);
    excluded.iter().map(|e| e.trim().to_uppercase()).filter(|e| !e.is_empty()).any(|e| {
        path.as_deref().is_some_and(|p| app_name_in_path(p.trim_end_matches(['/', '\\']), &e))
            || bundle.as_deref() == Some(e.as_str())
            || name.as_deref() == Some(e.as_str())
            || title.as_deref().is_some_and(|t| t.contains(&e))
    })
}

/// Whether a window of `kind` belonging to `app` may be snapped / tracked.
pub fn is_processable(s: &Settings, kind: WindowKind, app: &AppIdentity) -> bool {
    let kind_ok = match kind {
        WindowKind::Standard => true,
        WindowKind::Child => s.allow_snap_child_windows,
        WindowKind::Popup => s.allow_snap_popup_windows,
        WindowKind::Never => false,
    };
    kind_ok && !is_excluded(&s.excluded_apps, app)
}

/// macOS: classify a window from its `AXRole` / `AXSubrole` / `AXMinimized`.
pub fn mac_window_kind(role: Option<&str>, subrole: Option<&str>, minimized: bool) -> WindowKind {
    if minimized || role.is_some_and(|r| r != "AXWindow") {
        return WindowKind::Never;
    }
    match subrole {
        None | Some("AXStandardWindow") => WindowKind::Standard,
        Some("AXDialog") | Some("AXSystemDialog") => WindowKind::Child,
        Some("AXSheet") | Some("AXDrawer") => WindowKind::Never,
        Some(_) => WindowKind::Popup,
    }
}

/// Windows style bits relevant to [`win_window_kind`].
#[derive(Debug, Clone, Copy, Default)]
pub struct WinStyle {
    pub minimized: bool,
    pub visible: bool,
    pub tool_window: bool,
    pub is_root: bool,
    pub popup: bool,
    pub thick_frame: bool,
    pub caption: bool,
    pub min_max_box: bool,
    pub has_visible_owner: bool,
}

/// Windows: `FancyZonesWindowProcessing::DefineWindowType` (minus the
/// exclusion and virtual-desktop checks, which are done separately).
pub fn win_window_kind(st: WinStyle) -> WindowKind {
    if st.minimized || !st.visible || st.tool_window || !st.is_root {
        return WindowKind::Never;
    }
    if st.popup && !(st.thick_frame && (st.caption || st.min_max_box)) {
        return WindowKind::Popup;
    }
    if st.has_visible_owner {
        return WindowKind::Child;
    }
    WindowKind::Standard
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(path: &str, title: &str) -> AppIdentity {
        AppIdentity { path: Some(path.into()), title: Some(title.into()), ..Default::default() }
    }

    #[test]
    fn exclusion_matches_like_powertoys() {
        let ex = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let np = win(r"C:\Windows\System32\notepad.exe", "Untitled - Notepad");
        assert!(is_excluded(&ex(&["notepad.exe"]), &np));
        assert!(is_excluded(&ex(&["NOTEPAD"]), &np));
        assert!(is_excluded(&ex(&[r"system32\notepad.exe"]), &np));
        assert!(!is_excluded(&ex(&["pad.exe"]), &np));
        assert!(!is_excluded(&ex(&["calc.exe", ""]), &np));
        // title substring (PowerToys check_excluded_app_with_title)
        assert!(is_excluded(&ex(&["untitled"]), &np));
        // macOS identities
        let safari = AppIdentity {
            path: Some("/Applications/Safari.app".into()),
            bundle_id: Some("com.apple.Safari".into()),
            name: Some("Safari".into()),
            title: None,
        };
        assert!(is_excluded(&ex(&["com.apple.safari"]), &safari));
        assert!(is_excluded(&ex(&["safari"]), &safari));
        assert!(is_excluded(&ex(&["Safari.app"]), &safari));
        assert!(!is_excluded(&ex(&["com.apple"]), &safari));
        assert!(!is_excluded(&ex(&["Applications"]), &safari));
        assert!(!is_excluded(&[], &safari));
    }

    #[test]
    fn processability() {
        let mut s = Settings::default();
        let app = AppIdentity::from_path(Some("/Applications/TextEdit.app".into()));
        assert!(is_processable(&s, WindowKind::Standard, &app));
        assert!(!is_processable(&s, WindowKind::Child, &app));
        assert!(!is_processable(&s, WindowKind::Popup, &app));
        assert!(!is_processable(&s, WindowKind::Never, &app));
        s.allow_snap_child_windows = true;
        s.allow_snap_popup_windows = true;
        assert!(is_processable(&s, WindowKind::Child, &app));
        assert!(is_processable(&s, WindowKind::Popup, &app));
        s.excluded_apps = vec!["TextEdit".into()];
        assert!(!is_processable(&s, WindowKind::Standard, &app));
    }

    #[test]
    fn mac_kinds() {
        assert_eq!(mac_window_kind(Some("AXWindow"), Some("AXStandardWindow"), false), WindowKind::Standard);
        assert_eq!(mac_window_kind(Some("AXWindow"), None, false), WindowKind::Standard);
        assert_eq!(mac_window_kind(Some("AXWindow"), Some("AXDialog"), false), WindowKind::Child);
        assert_eq!(mac_window_kind(Some("AXWindow"), Some("AXFloatingWindow"), false), WindowKind::Popup);
        assert_eq!(mac_window_kind(Some("AXWindow"), Some("AXSheet"), false), WindowKind::Never);
        assert_eq!(mac_window_kind(Some("AXWindow"), Some("AXStandardWindow"), true), WindowKind::Never);
        assert_eq!(mac_window_kind(Some("AXButton"), None, false), WindowKind::Never);
    }

    #[test]
    fn windows_kinds() {
        let base = WinStyle { visible: true, is_root: true, thick_frame: true, caption: true, ..Default::default() };
        assert_eq!(win_window_kind(base), WindowKind::Standard);
        assert_eq!(win_window_kind(WinStyle { minimized: true, ..base }), WindowKind::Never);
        assert_eq!(win_window_kind(WinStyle { tool_window: true, ..base }), WindowKind::Never);
        assert_eq!(win_window_kind(WinStyle { is_root: false, ..base }), WindowKind::Never);
        // popup with thick frame + caption is a normal app (Calculator)
        assert_eq!(win_window_kind(WinStyle { popup: true, ..base }), WindowKind::Standard);
        assert_eq!(win_window_kind(WinStyle { popup: true, caption: false, ..base }), WindowKind::Popup);
        assert_eq!(
            win_window_kind(WinStyle { popup: true, caption: false, min_max_box: true, ..base }),
            WindowKind::Standard
        );
        assert_eq!(win_window_kind(WinStyle { has_visible_owner: true, ..base }), WindowKind::Child);
    }
}
