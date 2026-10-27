//! Snapshot of the current desktop (PowerToys' snapshot tool).

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::backend::{Backend, LiveWindow};
use crate::geometry::{self, Monitor};
use crate::model::{now_unix, App, AppOptions, OtherWindows, WindowState, Workspace};
use crate::store::new_id;

/// What a capture records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptureOptions {
    /// Bundle ids, app names or (Windows) exe names never captured.
    pub excluded_apps: Vec<String>,
    pub include_minimized: bool,
    /// Record the open document of document-based apps (`AXDocument`).
    pub documents: bool,
    /// Ask browsers for their front tab's URL through AppleScript. Off by
    /// default: the first use shows an Automation permission prompt per
    /// browser.
    pub browser_urls: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self { excluded_apps: vec![], include_minimized: true, documents: true, browser_urls: false }
    }
}

/// PowerMac itself (its settings and editor windows) is never captured.
fn is_powermac(w: &LiveWindow) -> bool {
    let b = w.app.bundle_id.to_ascii_lowercase();
    let file = w.app.path.rsplit(['/', '\\']).next().unwrap_or("").to_ascii_lowercase();
    b == "com.powermac.app" || b.starts_with("com.powermac.") || file == "powermac.app" || file == "powermac.exe"
}

fn excluded(w: &LiveWindow, list: &[String]) -> bool {
    if is_powermac(w) {
        return true;
    }
    list.iter().any(|e| {
        let e = e.trim();
        !e.is_empty()
            && (e.eq_ignore_ascii_case(&w.app.bundle_id)
                || e.eq_ignore_ascii_case(&w.app.name)
                || w.app.path.rsplit(['/', '\\']).next().is_some_and(|f| f.eq_ignore_ascii_case(e)))
    })
}

/// Build a workspace from the given monitors and windows.
pub fn from_windows(name: &str, monitors: Vec<Monitor>, windows: Vec<LiveWindow>, own_pid: i64, opts: &CaptureOptions) -> Workspace {
    let mut apps = Vec::new();
    for w in windows {
        if w.pid == own_pid || excluded(&w, &opts.excluded_apps) {
            continue;
        }
        if w.state == WindowState::Minimized && !opts.include_minimized {
            continue;
        }
        if w.frame.is_empty() {
            continue;
        }
        let mon = geometry::monitor_for_frame(&w.frame, &monitors);
        let mut state = w.state;
        if state == WindowState::Normal {
            if let Some(m) = mon {
                if w.frame.approx_eq(&m.work_area, 2) {
                    state = WindowState::Zoomed;
                }
            }
        }
        let name = if w.app.name.is_empty() {
            w.app.path.rsplit(['/', '\\']).next().unwrap_or("App").trim_end_matches(".app").trim_end_matches(".exe").to_string()
        } else {
            w.app.name.clone()
        };
        apps.push(App {
            id: new_id(),
            name,
            bundle_id: w.app.bundle_id.clone(),
            path: w.app.path.clone(),
            title: w.title.clone(),
            document: if opts.documents { w.document.clone() } else { None },
            url: if opts.browser_urls { w.url.clone() } else { None },
            command_line_args: String::new(),
            launch_as_admin: w.elevated,
            frame: w.frame,
            monitor_id: mon.map(|m| m.id.clone()).unwrap_or_default(),
            state,
            space: w.space,
            options: AppOptions::default(),
        });
    }
    Workspace {
        id: new_id(),
        name: name.to_string(),
        creation_time: now_unix(),
        last_launched_time: None,
        move_existing_windows: true,
        other_windows: OtherWindows::Leave,
        is_shortcut_needed: false,
        monitors,
        apps,
    }
}

/// Capture through a backend.
pub fn capture_with(backend: &dyn Backend, name: &str, opts: &CaptureOptions) -> Result<Workspace> {
    let monitors = backend.monitors()?;
    let windows = backend.windows()?;
    Ok(from_windows(name, monitors, windows, backend.own_pid(), opts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::launch::fake::Fake;

    fn monitors() -> Vec<Monitor> {
        vec![
            Monitor { id: "A".into(), name: "Built-in".into(), number: 0, frame: Rect::new(0, 0, 1512, 982), work_area: Rect::new(0, 33, 1512, 949), scale: 2.0, primary: true },
            Monitor { id: "B".into(), name: "Studio".into(), number: 1, frame: Rect::new(1512, 0, 2560, 1440), work_area: Rect::new(1512, 25, 2560, 1415), scale: 2.0, primary: false },
        ]
    }

    #[test]
    fn capture_records_windows() {
        let f = Fake::new(monitors());
        f.add_window("com.apple.TextEdit", 50, Rect::new(1600, 100, 800, 600), WindowState::Normal, Some("/tmp/a.txt"));
        f.add_window("com.apple.Safari", 51, Rect::new(0, 33, 1512, 949), WindowState::Normal, None);
        f.add_window("com.apple.Mail", 52, Rect::new(100, 100, 800, 600), WindowState::Minimized, None);
        f.add_window("com.powermac", 1, Rect::new(100, 100, 800, 600), WindowState::Normal, None);
        f.add_window("com.secret", 53, Rect::new(100, 100, 800, 600), WindowState::Normal, None);
        f.add_window("com.powermac.app", 54, Rect::new(100, 100, 800, 600), WindowState::Normal, None);
        let opts = CaptureOptions { excluded_apps: vec!["com.secret".into()], ..Default::default() };
        let ws = capture_with(&f, "Desk", &opts).unwrap();
        assert_eq!(ws.name, "Desk");
        assert_eq!(ws.monitors.len(), 2);
        let names: Vec<&str> = ws.apps.iter().map(|a| a.bundle_id.as_str()).collect();
        assert_eq!(names, vec!["com.apple.TextEdit", "com.apple.Safari", "com.apple.Mail"]);
        assert_eq!(ws.apps[0].monitor_id, "B");
        assert_eq!(ws.apps[0].document.as_deref(), Some("/tmp/a.txt"));
        assert_eq!(ws.apps[1].state, WindowState::Zoomed, "fills the work area");
        assert_eq!(ws.apps[2].state, WindowState::Minimized);
        assert_eq!(ws.apps[0].space, Some(1));

        let opts = CaptureOptions { include_minimized: false, documents: false, ..Default::default() };
        let ws = capture_with(&f, "Desk", &opts).unwrap();
        assert_eq!(ws.apps.len(), 3);
        assert!(ws.apps.iter().all(|a| a.document.is_none()));
    }

    #[test]
    fn capture_then_launch_round_trip() {
        // Capturing and launching right away moves nothing anywhere new.
        let f = Fake::new(monitors());
        f.add_window("ed", 50, Rect::new(1600, 100, 800, 600), WindowState::Normal, None);
        f.add_window("calc", 51, Rect::new(10, 40, 230, 400), WindowState::Normal, None);
        let ws = capture_with(&f, "x", &CaptureOptions::default()).unwrap();
        let r = crate::launch::run(&ws, &f, &crate::launch::LaunchOptions::default(), &|_| {}).unwrap();
        assert!(r.all_ok());
        assert!(f.launches.borrow().is_empty());
        for (id, p) in f.placed.borrow().iter() {
            let w = f.window(*id);
            assert_eq!(w.frame, p.frame);
        }
        assert_eq!(f.window(100).frame, Rect::new(1600, 100, 800, 600));
    }
}
