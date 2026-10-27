//! The data saved in `workspaces.json`.

use serde::{Deserialize, Serialize};

use crate::geometry::{Monitor, Rect};

/// Window state at capture time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowState {
    #[default]
    Normal,
    Minimized,
    /// Fills the work area (macOS "zoom", Windows "maximized").
    Zoomed,
    /// Native full screen (macOS: its own Space).
    FullScreen,
}

impl WindowState {
    pub const ALL: [WindowState; 4] = [WindowState::Normal, WindowState::Zoomed, WindowState::Minimized, WindowState::FullScreen];

    pub fn label(self) -> &'static str {
        match self {
            WindowState::Normal => "Normal",
            WindowState::Minimized => "Minimized",
            WindowState::Zoomed => if cfg!(windows) { "Maximized" } else { "Zoomed" },
            WindowState::FullScreen => "Full screen",
        }
    }
}

/// What to do with existing windows of an app when launching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExistingWindows {
    /// Use the workspace's `moveExistingWindows` setting.
    #[default]
    WorkspaceDefault,
    /// Reuse and move a running window of the app.
    Move,
    /// Leave running windows alone (only windows that appear after the
    /// launch are placed).
    Leave,
}

/// Per-app launch options.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppOptions {
    /// Always start a new instance (`open -n` on macOS) instead of reusing a
    /// running one. Most Mac apps are single-instance; this is for the few
    /// that are not (and for every app on Windows).
    pub launch_new_instance: bool,
    pub existing_windows: ExistingWindows,
    /// Wait this long before launching this app (ms).
    pub delay_ms: u32,
}

/// One window of a workspace ("application" in PowerToys).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct App {
    /// Unique within the workspace.
    pub id: String,
    /// Display name (`CFBundleName` / localized name; exe name on Windows).
    pub name: String,
    /// macOS bundle identifier (empty on Windows).
    pub bundle_id: String,
    /// `.app` bundle path (macOS) or executable path (Windows).
    pub path: String,
    /// Window title at capture time (for matching and display only).
    pub title: String,
    /// The window's document (file path) when the app reports one
    /// (`AXDocument`); it is opened with the app on launch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    /// The front tab's URL of a browser window (only with the AppleScript
    /// opt-in).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Command line arguments used on launch. Captured on Windows only when
    /// known (never on macOS, where other processes' arguments are not
    /// meaningful for GUI apps); editable. On macOS they are passed with
    /// `open --args` and only reach an app that is not running yet.
    pub command_line_args: String,
    /// Windows: launch elevated ("runas"). Ignored on macOS.
    pub launch_as_admin: bool,
    /// Global frame at capture time (the restored frame for minimized
    /// windows on macOS).
    pub frame: Rect,
    /// Id of the monitor the window was on ([`Workspace::monitors`]).
    pub monitor_id: String,
    pub state: WindowState,
    /// 1-based index of the Space (on its display) the window was on, when
    /// the private Space API reported one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<u32>,
    pub options: AppOptions,
}

impl App {
    /// Short description of where the window is saved.
    pub fn describe_frame(&self) -> String {
        format!("{}×{} at {}, {}", self.frame.width, self.frame.height, self.frame.x, self.frame.y)
    }

    /// File name of the document, if any.
    pub fn document_name(&self) -> Option<String> {
        let d = self.document.as_deref()?;
        let name = d.trim_end_matches('/').rsplit(['/', '\\']).next().unwrap_or(d);
        Some(name.to_string())
    }
}

/// What happens to windows that aren't part of the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OtherWindows {
    #[default]
    Leave,
    /// Minimize every other window.
    Minimize,
    /// Hide other apps (Cmd+H). Apps are never quit, so unsaved documents
    /// are never at risk.
    Hide,
}

impl OtherWindows {
    pub fn label(self) -> &'static str {
        match self {
            OtherWindows::Leave => "Leave",
            OtherWindows::Minimize => "Minimize",
            OtherWindows::Hide => "Hide apps",
        }
    }
}

/// A saved set of apps and window positions ("project" in PowerToys).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    /// Unix seconds.
    pub creation_time: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_launched_time: Option<i64>,
    /// Reuse and move running windows of the workspace's apps instead of
    /// launching new windows (per-app [`ExistingWindows`] can override).
    pub move_existing_windows: bool,
    pub other_windows: OtherWindows,
    /// PowerToys' "create desktop shortcut" flag (kept for round trips).
    pub is_shortcut_needed: bool,
    pub monitors: Vec<Monitor>,
    pub apps: Vec<App>,
}

impl Workspace {
    pub fn monitor(&self, id: &str) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.id == id)
    }

    /// Whether `app` reuses existing windows.
    pub fn reuses_existing(&self, app: &App) -> bool {
        if app.options.launch_new_instance {
            return false;
        }
        match app.options.existing_windows {
            ExistingWindows::WorkspaceDefault => self.move_existing_windows,
            ExistingWindows::Move => true,
            ExistingWindows::Leave => false,
        }
    }

    /// Number of distinct apps (an app with three windows counts once).
    pub fn distinct_apps(&self) -> usize {
        let mut seen: Vec<String> = Vec::new();
        for a in &self.apps {
            let k = crate::matching::app_key(a);
            if !seen.contains(&k) {
                seen.push(k);
            }
        }
        seen.len()
    }

    /// Drop monitors no app refers to.
    pub fn prune_monitors(&mut self) {
        let used: Vec<String> = self.apps.iter().map(|a| a.monitor_id.clone()).collect();
        self.monitors.retain(|m| used.contains(&m.id));
    }

    /// Give apps unique ids (after edits / imports).
    pub fn ensure_app_ids(&mut self) {
        let mut seen: Vec<String> = Vec::new();
        for a in &mut self.apps {
            if a.id.is_empty() || seen.contains(&a.id) {
                a.id = crate::store::new_id();
            }
            seen.push(a.id.clone());
        }
    }
}

/// Unix time now (seconds).
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "just now", "5 minutes ago", "3 days ago", …
pub fn relative_time(then: i64, now: i64) -> String {
    let d = (now - then).max(0);
    let plural = |n: i64, unit: &str| if n == 1 { format!("1 {unit} ago") } else { format!("{n} {unit}s ago") };
    match d {
        0..=59 => "just now".into(),
        60..=3599 => plural(d / 60, "minute"),
        3600..=86_399 => plural(d / 3600, "hour"),
        86_400..=2_591_999 => plural(d / 86_400, "day"),
        2_592_000..=31_535_999 => plural(d / 2_592_000, "month"),
        _ => plural(d / 31_536_000, "year"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times() {
        assert_eq!(relative_time(100, 110), "just now");
        assert_eq!(relative_time(0, 60), "1 minute ago");
        assert_eq!(relative_time(0, 7200), "2 hours ago");
        assert_eq!(relative_time(0, 86_400 * 3), "3 days ago");
        assert_eq!(relative_time(10, 0), "just now");
    }

    #[test]
    fn reuse_rules() {
        let ws = Workspace { move_existing_windows: true, ..Default::default() };
        let mut a = App::default();
        assert!(ws.reuses_existing(&a));
        a.options.existing_windows = ExistingWindows::Leave;
        assert!(!ws.reuses_existing(&a));
        a.options.existing_windows = ExistingWindows::Move;
        assert!(ws.reuses_existing(&a));
        a.options.launch_new_instance = true;
        assert!(!ws.reuses_existing(&a));
    }

    #[test]
    fn document_name() {
        let a = App { document: Some("/Users/me/Notes/todo.txt".into()), ..Default::default() };
        assert_eq!(a.document_name().as_deref(), Some("todo.txt"));
    }
}
