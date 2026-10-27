//! Import PowerToys' `workspaces.json`
//! (`%LOCALAPPDATA%\Microsoft\PowerToys\Workspaces\workspaces.json`).
//!
//! On Windows the entries are used as they are. Elsewhere the Windows paths
//! don't resolve, so each app is mapped to an installed app by name (exe
//! stem, PowerToys' "PWA (Browser)" names and a small alias table such as
//! Notepad → TextEdit); entries without a match are reported and left out.
//! PowerToys' monitors are paired with the current ones by position
//! (primary first, then left to right), and window positions are carried
//! over relative to their monitor.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::geometry::{Monitor, Rect};
use crate::matching::{self, InstalledApp};
use crate::model::{App, AppOptions, OtherWindows, WindowState, Workspace};

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct PtFile {
    workspaces: Vec<PtProject>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
struct PtProject {
    id: String,
    name: String,
    creation_time: i64,
    last_launched_time: Option<i64>,
    is_shortcut_needed: bool,
    move_existing_windows: bool,
    monitor_configuration: Vec<PtMonitor>,
    applications: Vec<PtApp>,
}

#[derive(Debug, Deserialize, Default, Clone, Copy)]
#[serde(default)]
struct PtRect {
    top: i32,
    left: i32,
    width: i32,
    height: i32,
}

impl PtRect {
    fn rect(&self) -> Rect {
        Rect::new(self.left, self.top, self.width, self.height)
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default, rename_all = "kebab-case")]
struct PtMonitor {
    id: String,
    instance_id: String,
    monitor_number: u32,
    dpi: u32,
    monitor_rect_dpi_aware: PtRect,
    monitor_rect_dpi_unaware: PtRect,
}

#[derive(Debug, Deserialize, Default, Clone, Copy)]
#[serde(default)]
struct PtPosition {
    #[serde(rename = "X")]
    x: i32,
    #[serde(rename = "Y")]
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default, rename_all = "kebab-case")]
struct PtApp {
    id: String,
    application: String,
    application_path: String,
    title: String,
    pwa_app_id: String,
    command_line_arguments: String,
    is_elevated: bool,
    minimized: bool,
    maximized: bool,
    position: PtPosition,
    monitor: u32,
}

/// What the import needs to know about this machine.
#[derive(Debug, Clone, Default)]
pub struct ImportContext {
    /// Installed apps to map Windows entries to (ignored when `native`).
    pub installed: Vec<InstalledApp>,
    /// The current monitors (PowerToys' monitors are paired with these).
    pub monitors: Vec<Monitor>,
    /// Keep Windows paths / arguments as they are (importing on Windows).
    pub native: bool,
    /// Ids already in use (imported ids that collide get new ones).
    pub existing_ids: Vec<String>,
}

impl ImportContext {
    /// The context for this machine.
    pub fn current() -> Self {
        Self {
            installed: if cfg!(windows) { vec![] } else { crate::platform::installed_apps() },
            monitors: crate::platform::monitors().unwrap_or_default(),
            native: cfg!(windows),
            existing_ids: vec![],
        }
    }
}

/// Result of an import.
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub workspaces: Vec<Workspace>,
    /// (workspace name, app name) of entries without an installed app.
    pub unmatched: Vec<(String, String)>,
    /// Workspaces left out because none of their apps matched.
    pub skipped: Vec<String>,
}

impl ImportReport {
    pub fn summary(&self) -> String {
        let mut s = format!(
            "Imported {} workspace{}",
            self.workspaces.len(),
            if self.workspaces.len() == 1 { "" } else { "s" }
        );
        if !self.unmatched.is_empty() {
            let mut names: Vec<&str> = self.unmatched.iter().map(|(_, a)| a.as_str()).collect();
            names.sort();
            names.dedup();
            s.push_str(&format!("; not found on this Mac: {}", names.join(", ")));
        }
        if !self.skipped.is_empty() {
            s.push_str(&format!("; skipped (no apps found): {}", self.skipped.join(", ")));
        }
        s
    }
}

/// Order for pairing: the primary (at 0,0) first, then left to right, top
/// to bottom.
fn pairing_order<T>(items: &mut [T], rect: impl Fn(&T) -> Rect, primary: impl Fn(&T) -> bool) {
    items.sort_by_key(|m| {
        let r = rect(m);
        (!primary(m), r.x, r.y)
    });
}

fn exe_stem(path: &str) -> String {
    let f = path.rsplit(['\\', '/']).next().unwrap_or(path);
    f.strip_suffix(".exe").or_else(|| f.strip_suffix(".EXE")).unwrap_or(f).to_string()
}

pub fn import_powertoys(json: &str, ctx: &ImportContext) -> Result<ImportReport> {
    let file: PtFile = serde_json::from_str(json).context("not a PowerToys workspaces.json")?;
    let mut report = ImportReport::default();
    let mut used_ids = ctx.existing_ids.clone();

    let mut current = ctx.monitors.clone();
    pairing_order(&mut current, |m| m.frame, |m| m.primary);

    for p in file.workspaces {
        let mut pt_mons = p.monitor_configuration.clone();
        pairing_order(
            &mut pt_mons,
            |m| m.monitor_rect_dpi_unaware.rect(),
            |m| m.monitor_rect_dpi_unaware.left == 0 && m.monitor_rect_dpi_unaware.top == 0,
        );
        // PowerToys monitor number -> (our monitor, PowerToys physical rect)
        let mut mons: Vec<(u32, Monitor, Rect)> = Vec::new();
        for (i, pm) in pt_mons.iter().enumerate() {
            let phys = if pm.monitor_rect_dpi_aware.width > 0 { pm.monitor_rect_dpi_aware.rect() } else { pm.monitor_rect_dpi_unaware.rect() };
            let m = match current.get(i) {
                Some(c) => c.clone(),
                None => {
                    // More monitors than here: keep it (launch falls back).
                    let logical = pm.monitor_rect_dpi_unaware.rect();
                    let id = if pm.instance_id.is_empty() { pm.id.clone() } else { pm.instance_id.clone() };
                    Monitor {
                        id: if id.is_empty() { format!("powertoys-{}", pm.monitor_number) } else { id },
                        name: format!("Display {}", pm.monitor_number),
                        number: pm.monitor_number,
                        frame: logical,
                        work_area: logical,
                        scale: if pm.dpi > 0 { pm.dpi as f64 / 96.0 } else { 1.0 },
                        primary: false,
                    }
                }
            };
            mons.push((pm.monitor_number, m, phys));
        }

        let mut apps = Vec::new();
        for a in &p.applications {
            let (name, bundle_id, path, args) = if ctx.native {
                let n = if a.application.is_empty() { exe_stem(&a.application_path) } else { a.application.clone() };
                (n, String::new(), a.application_path.clone(), a.command_line_arguments.clone())
            } else {
                match matching::find_installed(&a.application, &a.application_path, &ctx.installed) {
                    Some(i) => (i.name.clone(), i.bundle_id.clone(), i.path.clone(), String::new()),
                    None => {
                        let n = if a.application.is_empty() { exe_stem(&a.application_path) } else { a.application.clone() };
                        report.unmatched.push((p.name.clone(), n));
                        continue;
                    }
                }
            };
            let entry = mons.iter().find(|(n, _, _)| *n == a.monitor).or_else(|| mons.first());
            let pos = Rect::new(a.position.x, a.position.y, a.position.width, a.position.height);
            let (frame, monitor_id) = match entry {
                Some((_, m, phys)) => {
                    let target = if m.work_area.is_empty() { m.frame } else { m.work_area };
                    let f = if a.maximized {
                        target
                    } else if a.minimized {
                        // PowerToys stores the monitor rect for minimized windows.
                        let w = target.width * 7 / 10;
                        let h = target.height * 7 / 10;
                        Rect::new(target.x + (target.width - w) / 2, target.y + (target.height - h) / 2, w, h)
                    } else {
                        crate::geometry::scale_between(&pos, phys, &m.frame).fit_into(&target)
                    };
                    (f, m.id.clone())
                }
                None => (pos, String::new()),
            };
            let state = if a.minimized {
                WindowState::Minimized
            } else if a.maximized {
                WindowState::Zoomed
            } else {
                WindowState::Normal
            };
            let mut id = a.id.clone();
            if id.is_empty() || apps.iter().any(|x: &App| x.id == id) {
                id = crate::store::new_id();
            }
            apps.push(App {
                id,
                name,
                bundle_id,
                path,
                title: a.title.clone(),
                document: None,
                url: None,
                command_line_args: args,
                launch_as_admin: ctx.native && a.is_elevated,
                frame,
                monitor_id,
                state,
                space: None,
                options: AppOptions::default(),
            });
            let _ = &a.pwa_app_id;
        }
        if apps.is_empty() {
            report.skipped.push(p.name.clone());
            continue;
        }
        let mut id = p.id.clone();
        if id.is_empty() || used_ids.contains(&id) {
            id = crate::store::new_id();
        }
        used_ids.push(id.clone());
        let mut ws = Workspace {
            id,
            name: p.name.clone(),
            creation_time: if p.creation_time > 0 { p.creation_time } else { crate::model::now_unix() },
            last_launched_time: p.last_launched_time.filter(|t| *t > 0),
            move_existing_windows: p.move_existing_windows,
            other_windows: OtherWindows::Leave,
            is_shortcut_needed: p.is_shortcut_needed,
            monitors: mons.into_iter().map(|(_, m, _)| m).collect(),
            apps,
        };
        ws.prune_monitors();
        report.workspaces.push(ws);
    }
    Ok(report)
}

/// Where PowerToys keeps the file on this machine (Windows only).
pub fn default_powertoys_path() -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join(r"Microsoft\PowerToys\Workspaces\workspaces.json"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const SAMPLE: &str = r#"{
      "workspaces": [
        {
          "id": "{2D7A6C3E-1111-4A3B-9E8F-0123456789AB}",
          "name": "Morning",
          "creation-time": 1718000000,
          "last-launched-time": 1718100000,
          "is-shortcut-needed": false,
          "move-existing-windows": true,
          "monitor-configuration": [
            {"id": "\\\\?\\DISPLAY#DELA0A7#5&1", "instance-id": "5&1a2b", "monitor-number": 2, "dpi": 96,
             "monitor-rect-dpi-aware": {"top": 0, "left": 2560, "width": 1920, "height": 1080},
             "monitor-rect-dpi-unaware": {"top": 0, "left": 1707, "width": 1280, "height": 720}},
            {"id": "\\\\?\\DISPLAY#BOE0900#4&2", "instance-id": "4&2c3d", "monitor-number": 1, "dpi": 144,
             "monitor-rect-dpi-aware": {"top": 0, "left": 0, "width": 2560, "height": 1600},
             "monitor-rect-dpi-unaware": {"top": 0, "left": 0, "width": 1707, "height": 1067}}
          ],
          "applications": [
            {"id": "{A1}", "application": "Notepad", "application-path": "C:\\Windows\\System32\\notepad.exe",
             "title": "todo.txt - Notepad", "package-full-name": "", "app-user-model-id": "", "pwa-app-id": "",
             "command-line-arguments": "C:\\todo.txt", "is-elevated": false, "can-launch-elevated": true,
             "minimized": false, "maximized": false,
             "position": {"X": 0, "Y": 0, "width": 1280, "height": 1600}, "monitor": 1, "version": "1"},
            {"id": "{A2}", "application": "Visual Studio Code", "application-path": "C:\\Users\\me\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe",
             "title": "proj", "command-line-arguments": "", "is-elevated": true,
             "minimized": false, "maximized": true,
             "position": {"X": 2560, "Y": 0, "width": 1920, "height": 1040}, "monitor": 2},
            {"id": "{A3}", "application": "Paint.NET", "application-path": "C:\\Program Files\\paint.net\\paintdotnet.exe",
             "title": "img", "command-line-arguments": "", "minimized": true, "maximized": false,
             "position": {"X": 0, "Y": 0, "width": 2560, "height": 1600}, "monitor": 1}
          ]
        },
        {
          "id": "{2D7A6C3E-2222-4A3B-9E8F-0123456789AB}",
          "name": "Games",
          "creation-time": 1718000001,
          "monitor-configuration": [],
          "applications": [
            {"application": "Steam", "application-path": "C:\\Steam\\steam.exe", "title": "Steam",
             "command-line-arguments": "", "minimized": false, "maximized": false,
             "position": {"X": 0, "Y": 0, "width": 800, "height": 600}, "monitor": 1}
          ]
        }
      ]
    }"#;

    fn mac_monitors() -> Vec<Monitor> {
        vec![
            Monitor {
                id: "EXT".into(),
                name: "Studio Display".into(),
                number: 1,
                frame: Rect::new(1512, 0, 2560, 1440),
                work_area: Rect::new(1512, 25, 2560, 1415),
                scale: 2.0,
                primary: false,
            },
            Monitor {
                id: "BUILTIN".into(),
                name: "Built-in".into(),
                number: 0,
                frame: Rect::new(0, 0, 1512, 982),
                work_area: Rect::new(0, 33, 1512, 949),
                scale: 2.0,
                primary: true,
            },
        ]
    }

    fn installed() -> Vec<InstalledApp> {
        vec![
            InstalledApp { name: "TextEdit".into(), bundle_id: "com.apple.TextEdit".into(), path: "/System/Applications/TextEdit.app".into() },
            InstalledApp { name: "Visual Studio Code".into(), bundle_id: "com.microsoft.VSCode".into(), path: "/Applications/Visual Studio Code.app".into() },
        ]
    }

    #[test]
    fn import_on_mac_maps_by_name() {
        let ctx = ImportContext { installed: installed(), monitors: mac_monitors(), native: false, existing_ids: vec![] };
        let r = import_powertoys(SAMPLE, &ctx).unwrap();
        assert_eq!(r.workspaces.len(), 1);
        assert_eq!(r.skipped, vec!["Games".to_string()]);
        let mut un: Vec<&str> = r.unmatched.iter().map(|(_, a)| a.as_str()).collect();
        un.sort();
        assert_eq!(un, vec!["Paint.NET", "Steam"]);
        let ws = &r.workspaces[0];
        assert_eq!(ws.name, "Morning");
        assert!(ws.move_existing_windows);
        assert_eq!(ws.last_launched_time, Some(1718100000));
        assert_eq!(ws.apps.len(), 2);
        let te = &ws.apps[0];
        assert_eq!(te.name, "TextEdit");
        assert_eq!(te.bundle_id, "com.apple.TextEdit");
        assert_eq!(te.command_line_args, "", "Windows arguments are dropped on macOS");
        // Left half of PowerToys' primary (monitor 1 at 0,0) -> left half of the Mac's primary.
        assert_eq!(te.monitor_id, "BUILTIN");
        assert_eq!(te.frame, Rect::new(0, 33, 756, 949));
        let code = &ws.apps[1];
        assert_eq!(code.monitor_id, "EXT");
        assert_eq!(code.state, WindowState::Zoomed);
        assert_eq!(code.frame, Rect::new(1512, 25, 2560, 1415));
        assert!(!code.launch_as_admin);
        // Both monitors are referenced, so both stay.
        assert_eq!(ws.monitors.len(), 2);
        assert!(r.summary().contains("not found on this Mac: Paint.NET, Steam"), "{}", r.summary());
    }

    #[test]
    fn import_native_keeps_everything() {
        let ctx = ImportContext { installed: vec![], monitors: vec![], native: true, existing_ids: vec!["{2D7A6C3E-1111-4A3B-9E8F-0123456789AB}".into()] };
        let r = import_powertoys(SAMPLE, &ctx).unwrap();
        assert_eq!(r.workspaces.len(), 2);
        assert!(r.unmatched.is_empty());
        let ws = &r.workspaces[0];
        assert_ne!(ws.id, "{2D7A6C3E-1111-4A3B-9E8F-0123456789AB}", "colliding id replaced");
        assert_eq!(ws.apps.len(), 3);
        assert_eq!(ws.apps[0].command_line_args, r"C:\todo.txt");
        assert_eq!(ws.apps[0].path, r"C:\Windows\System32\notepad.exe");
        assert!(ws.apps[1].launch_as_admin);
        assert_eq!(ws.apps[2].state, WindowState::Minimized);
        // Without current monitors the PowerToys monitors are kept.
        assert!(ws.monitors.iter().any(|m| m.id == "4&2c3d"));
        // A project without monitor info still imports (frame kept).
        assert_eq!(r.workspaces[1].apps[0].frame, Rect::new(0, 0, 800, 600));
        assert_eq!(r.workspaces[1].apps[0].name, "Steam");
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(import_powertoys("not json", &ImportContext::default()).is_err());
        let r = import_powertoys("{}", &ImportContext::default()).unwrap();
        assert!(r.workspaces.is_empty());
    }
}
