//! macOS backend: CGWindowList (z-order, every Space) + Accessibility
//! (titles, frames, states, documents, moving) + NSWorkspace (apps) +
//! `open` (launching without activating) + private CGS calls for Spaces.

mod apps;
mod ax;
mod ffi;
mod screens;
mod spaces;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use self::ax::AxElement;
use self::ffi::*;
use crate::backend::{AppIdentity, Backend, LaunchRequest, LiveWindow, Placement};
use crate::capture::CaptureOptions;
use crate::geometry::{Monitor, Rect};
use crate::model::WindowState;

pub use apps::installed_apps;

pub fn has_permissions() -> bool {
    ax::is_trusted()
}

pub fn monitors() -> Result<Vec<Monitor>> {
    screens::monitors()
}

/// App icon as premultiplied RGBA (`size`×`size`).
pub fn app_icon_rgba(path: &str, bundle_id: &str, size: u32) -> Option<Vec<u8>> {
    apps::icon_rgba(path, bundle_id, size)
}

pub fn app_exists(path: &str, bundle_id: &str) -> bool {
    apps::app_exists(path, bundle_id)
}

/// Show a file chooser (blocking). Uses `osascript`'s `choose file`, which
/// needs no Automation permission (it doesn't control another app).
pub fn pick_file(prompt: &str) -> Option<PathBuf> {
    let script = format!("POSIX path of (choose file with prompt \"{}\")", prompt.replace('"', "'"));
    let out = std::process::Command::new("/usr/bin/osascript").arg("-e").arg(script).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!p.is_empty()).then(|| PathBuf::from(p))
}

/// Reveal a file in Finder.
pub fn reveal(path: &Path) {
    let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(path).spawn().map(|mut c| c.wait());
}

/// Make the editor a regular app (Dock icon, can be activated).
pub fn become_foreground_app() {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

/// One CGWindowList entry.
#[derive(Debug, Clone)]
struct CgWindow {
    wid: u32,
    pid: i32,
    bounds: Rect,
    onscreen: bool,
    name: Option<String>,
}

fn cg_windows() -> Vec<CgWindow> {
    let mut out = Vec::new();
    unsafe {
        let arr = CGWindowListCopyWindowInfo(kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements, kCGNullWindowID);
        let Some(_owned) = CfOwned::new(arr) else { return out };
        for i in 0..CFArrayGetCount(arr) {
            let d = CFArrayGetValueAtIndex(arr, i);
            if d.is_null() || CFGetTypeID(d) != CFDictionaryGetTypeID() {
                continue;
            }
            if dict_i64(d, "kCGWindowLayer") != Some(0) {
                continue;
            }
            let (Some(wid), Some(pid)) = (dict_i64(d, "kCGWindowNumber"), dict_i64(d, "kCGWindowOwnerPID")) else { continue };
            let mut r = CGRect::default();
            let b = dict_get(d, "kCGWindowBounds");
            if b.is_null() || !CGRectMakeWithDictionaryRepresentation(b, &mut r) {
                continue;
            }
            out.push(CgWindow {
                wid: wid as u32,
                pid: pid as i32,
                bounds: Rect::new(r.origin.x.round() as i32, r.origin.y.round() as i32, r.size.width.round() as i32, r.size.height.round() as i32),
                onscreen: dict_bool(d, "kCGWindowIsOnscreen").unwrap_or(false),
                name: dict_string(d, "kCGWindowName").filter(|s| !s.is_empty()),
            });
        }
    }
    out
}

/// Standard windows; minimized windows report the `AXDialog` subrole on
/// recent macOS, so those count too.
fn is_app_window(w: &AxElement) -> bool {
    match w.subrole().as_deref() {
        Some("AXStandardWindow") => true,
        Some("AXDialog") => w.bool_attr("AXMinimized").unwrap_or(false),
        _ => false,
    }
}

const MIN_W: i32 = 60;
const MIN_H: i32 = 40;

/// Browsers whose front tab URL can be read with AppleScript.
const BROWSERS: &[(&str, bool)] = &[
    // (bundle id, Chromium-style dictionary)
    ("com.apple.Safari", false),
    ("com.apple.SafariTechnologyPreview", false),
    ("com.google.Chrome", true),
    ("com.microsoft.edgemac", true),
    ("com.brave.Browser", true),
    ("com.vivaldi.Vivaldi", true),
];

/// (window title, front tab URL) of a running browser. Triggers the
/// Automation prompt the first time: only called with the opt-in.
fn browser_tabs(bundle_id: &str, chromium: bool) -> Vec<(String, String)> {
    let (title, url) = if chromium { ("title of active tab of w", "URL of active tab of w") } else { ("name of w", "URL of current tab of w") };
    let script = format!(
        "set out to \"\"\ntell application id \"{bundle_id}\"\nrepeat with w in windows\ntry\nset out to out & ({title}) & tab & ({url}) & linefeed\nend try\nend repeat\nend tell\nreturn out"
    );
    let Ok(o) = std::process::Command::new("/usr/bin/osascript").arg("-e").arg(script).output() else { return vec![] };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let (t, u) = l.split_once('\t')?;
            (!u.trim().is_empty() && u != "missing value").then(|| (t.to_string(), u.trim().to_string()))
        })
        .collect()
}

/// The native window system.
pub struct Native {
    documents: bool,
    browser_urls: bool,
    /// wid -> (pid, element)
    elements: Mutex<HashMap<u32, (i32, AxElement)>>,
    /// Off-screen windows the remote-token probe couldn't resolve.
    failed: Mutex<HashSet<u32>>,
    start: Instant,
}

impl Native {
    pub fn new(opts: &CaptureOptions) -> Self {
        Self {
            documents: opts.documents,
            browser_urls: opts.browser_urls,
            elements: Mutex::new(HashMap::new()),
            failed: Mutex::new(HashSet::new()),
            start: Instant::now(),
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<u32, (i32, AxElement)>> {
        self.elements.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn element(&self, w: &LiveWindow) -> Result<AxElement> {
        let wid = w.id as u32;
        if let Some((_, e)) = self.cache().get(&wid) {
            return Ok(e.clone());
        }
        let pid = w.pid as i32;
        let found = AxElement::application(pid)
            .elements_attr("AXWindows")
            .into_iter()
            .find(|e| e.window_id() == Some(wid))
            .or_else(|| ax::windows_by_remote_token(pid, &[wid], 1000).into_iter().next().map(|(_, e)| e));
        match found {
            Some(e) => {
                self.cache().insert(wid, (pid, e.clone()));
                Ok(e)
            }
            None => bail!("window {wid} is gone"),
        }
    }

    fn build(&self, cg: &CgWindow, w: &AxElement, app: &apps::AppInfo) -> Option<LiveWindow> {
        let minimized = w.bool_attr("AXMinimized").unwrap_or(false);
        let fullscreen = w.bool_attr("AXFullScreen").unwrap_or(false);
        let frame = w.frame().unwrap_or(cg.bounds);
        if !minimized && (frame.width < MIN_W || frame.height < MIN_H) {
            return None;
        }
        let title = w.string_attr("AXTitle").filter(|t| !t.is_empty()).or_else(|| cg.name.clone()).unwrap_or_default();
        let document = if self.documents {
            w.string_attr("AXDocument").map(|d| ax::document_path(&d)).filter(|d| d.starts_with('/'))
        } else {
            None
        };
        Some(LiveWindow {
            id: cg.wid as u64,
            pid: app.pid as i64,
            app: AppIdentity { bundle_id: app.bundle_id.clone(), path: app.path.clone(), name: app.name.clone() },
            title,
            frame,
            state: if minimized {
                WindowState::Minimized
            } else if fullscreen {
                WindowState::FullScreen
            } else {
                WindowState::Normal
            },
            space: None,
            document,
            url: None,
            app_hidden: app.hidden,
            elevated: false,
        })
    }

    /// Press a window's close button (manual tests only: Workspaces never
    /// closes windows).
    #[doc(hidden)]
    pub fn close_window_for_tests(&self, w: &LiveWindow) -> Result<()> {
        match self.element(w)?.element_attr("AXCloseButton") {
            Some(b) => b.perform("AXPress"),
            None => bail!("no close button"),
        }
    }

    /// The CGDirectDisplayID of a monitor id.
    fn display_of(&self, monitor_id: Option<&str>) -> Option<u32> {
        let id = monitor_id?;
        screens::monitors_with_displays().ok()?.into_iter().find(|(m, _)| m.id == id).map(|(_, d)| d)
    }
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Split `a "b c" d` into arguments (double quotes group, no escapes).
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The `open` arguments for a launch request.
pub(crate) fn open_args(req: &LaunchRequest, exists: impl Fn(&str) -> bool) -> Vec<String> {
    let mut a: Vec<String> = vec!["-g".into()];
    if req.new_instance {
        a.push("-n".into());
    }
    if !req.path.is_empty() && exists(&req.path) {
        a.push("-a".into());
        a.push(req.path.clone());
    } else if !req.bundle_id.is_empty() {
        a.push("-b".into());
        a.push(req.bundle_id.clone());
    } else {
        a.push("-a".into());
        a.push(req.name.clone());
    }
    if let Some(o) = &req.open {
        let is_url = o.contains("://");
        if is_url || exists(o) {
            a.push(o.clone());
        } else {
            log::info!("workspaces: {o} no longer exists; launching {} without it", req.name);
        }
    }
    let args = split_args(&req.args);
    if !args.is_empty() {
        a.push("--args".into());
        a.extend(args);
    }
    a
}

impl Backend for Native {
    fn monitors(&self) -> Result<Vec<Monitor>> {
        screens::monitors()
    }

    fn windows(&self) -> Result<Vec<LiveWindow>> {
        if !ax::is_trusted() {
            bail!("PowerMac needs the Accessibility permission (System Settings → Privacy & Security → Accessibility)");
        }
        let own = std::process::id() as i32;
        let cg = cg_windows();
        let cg_index: HashMap<u32, usize> = cg.iter().enumerate().map(|(i, w)| (w.wid, i)).collect();
        let mut pids: Vec<i32> = cg.iter().map(|w| w.pid).filter(|p| *p != own).collect();
        pids.sort_unstable();
        pids.dedup();
        let all_apps: Vec<apps::AppInfo> = apps::regular_apps_for(&pids);
        let mut found: Vec<(usize, LiveWindow)> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        let mut probe: HashMap<i32, Vec<u32>> = HashMap::new();

        for app in &all_apps {
            for w in AxElement::application(app.pid).elements_attr("AXWindows") {
                let Some(wid) = w.window_id() else { continue };
                if w.role().as_deref() != Some("AXWindow") || !is_app_window(&w) {
                    continue;
                }
                let Some(&ci) = cg_index.get(&wid) else { continue };
                if let Some(info) = self.build(&cg[ci], &w, app) {
                    seen.insert(wid);
                    self.cache().insert(wid, (app.pid, w));
                    found.push((ci, info));
                }
            }
            if !app.hidden {
                let cands: Vec<u32> = cg
                    .iter()
                    .filter(|c| c.pid == app.pid && !c.onscreen && !seen.contains(&c.wid) && c.bounds.width >= MIN_W && c.bounds.height >= MIN_H)
                    .map(|c| c.wid)
                    .collect();
                if !cands.is_empty() {
                    probe.insert(app.pid, cands);
                }
            }
        }

        // Windows on other Spaces aren't in AXWindows.
        if !probe.is_empty() {
            let all: Vec<u32> = probe.values().flatten().copied().collect();
            let hidden = spaces::on_hidden_spaces(&all);
            for (pid, cands) in probe {
                let Some(app) = all_apps.iter().find(|a| a.pid == pid) else { continue };
                let cands: Vec<u32> = cands.into_iter().filter(|w| hidden.as_ref().is_none_or(|h| h.contains(w))).collect();
                let unknown: Vec<u32> = {
                    let cache = self.cache();
                    let failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
                    cands.iter().copied().filter(|w| !cache.contains_key(w) && !failed.contains(w)).collect()
                };
                if !unknown.is_empty() {
                    let got = ax::windows_by_remote_token(pid, &unknown, 1000);
                    let mut failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
                    for w in &unknown {
                        if !got.iter().any(|(g, _)| g == w) {
                            failed.insert(*w);
                        }
                    }
                    let mut cache = self.cache();
                    for (wid, e) in got {
                        cache.insert(wid, (pid, e));
                    }
                }
                for wid in cands {
                    let Some(w) = self.cache().get(&wid).map(|(_, e)| e.clone()) else { continue };
                    if !is_app_window(&w) {
                        continue;
                    }
                    let Some(&ci) = cg_index.get(&wid) else { continue };
                    if let Some(info) = self.build(&cg[ci], &w, app) {
                        seen.insert(wid);
                        found.push((ci, info));
                    }
                }
            }
        }

        self.cache().retain(|w, _| cg_index.contains_key(w));
        self.failed.lock().unwrap_or_else(|e| e.into_inner()).retain(|w| cg_index.contains_key(w));

        found.sort_by_key(|(i, _)| *i);
        let mut out: Vec<LiveWindow> = found.into_iter().map(|(_, w)| w).collect();
        let wids: Vec<u32> = out.iter().map(|w| w.id as u32).collect();
        let idx = spaces::space_indexes(&wids);
        for w in &mut out {
            w.space = idx.get(&(w.id as u32)).copied();
        }
        if self.browser_urls {
            for (bid, chromium) in BROWSERS {
                if !out.iter().any(|w| w.app.bundle_id == *bid) {
                    continue;
                }
                let tabs = browser_tabs(bid, *chromium);
                for w in out.iter_mut().filter(|w| w.app.bundle_id == *bid) {
                    w.url = tabs
                        .iter()
                        .find(|(t, _)| !t.is_empty() && (*t == w.title || w.title.starts_with(t.as_str())))
                        .map(|(_, u)| u.clone());
                }
            }
        }
        Ok(out)
    }

    fn launch(&self, req: &LaunchRequest) -> Result<()> {
        let args = open_args(req, |p| Path::new(p).exists());
        log::info!("workspaces: open {}", args.join(" "));
        let out = std::process::Command::new("/usr/bin/open").args(&args).output().context("running open")?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            bail!("{}", if err.is_empty() { format!("open exited with {}", out.status) } else { err });
        }
        Ok(())
    }

    fn place(&self, w: &LiveWindow, p: &Placement) -> Result<Option<String>> {
        let el = self.element(w)?;
        let pid = w.pid as i32;
        let mut notes: Vec<String> = Vec::new();
        if p.state != WindowState::Minimized && apps::is_hidden(pid) {
            apps::set_hidden(pid, false);
        }
        if p.state != WindowState::Minimized && el.bool_attr("AXMinimized").unwrap_or(false) {
            let _ = el.set_bool_attr("AXMinimized", false);
            wait_until(Duration::from_millis(1500), || !el.bool_attr("AXMinimized").unwrap_or(false));
        }
        if p.state != WindowState::FullScreen && el.bool_attr("AXFullScreen").unwrap_or(false) {
            let _ = el.set_bool_attr("AXFullScreen", false);
            wait_until(Duration::from_secs(3), || !el.bool_attr("AXFullScreen").unwrap_or(false));
            // The exit animation keeps resizing for a moment.
            std::thread::sleep(Duration::from_millis(700));
        }
        if let Some(space) = p.space {
            if p.state != WindowState::FullScreen && !spaces::move_to_space(w.id as u32, self.display_of(p.monitor_id.as_deref()), space) {
                notes.push(format!("could not move it to Space {space}"));
            }
        }
        if p.state != WindowState::FullScreen || !el.bool_attr("AXFullScreen").unwrap_or(false) {
            el.set_frame(pid, p.frame)?;
        }
        match p.state {
            WindowState::Minimized => {
                el.set_bool_attr("AXMinimized", true)?;
            }
            WindowState::FullScreen => {
                if !el.bool_attr("AXFullScreen").unwrap_or(false) && el.set_bool_attr("AXFullScreen", true).is_err() {
                    match el.element_attr("AXFullScreenButton") {
                        Some(b) => b.perform("AXPress")?,
                        None => notes.push("the app doesn't support full screen".into()),
                    }
                }
            }
            WindowState::Normal | WindowState::Zoomed => {}
        }
        Ok((!notes.is_empty()).then(|| notes.join("; ")))
    }

    fn window_frame(&self, w: &LiveWindow) -> Option<Rect> {
        self.element(w).ok()?.frame().ok()
    }

    fn minimize(&self, w: &LiveWindow) -> Result<()> {
        self.element(w)?.set_bool_attr("AXMinimized", true)
    }

    fn hide_app(&self, pid: i64) -> Result<()> {
        if !apps::set_hidden(pid as i32, true) {
            bail!("could not hide pid {pid}");
        }
        Ok(())
    }

    fn activate(&self, w: &LiveWindow) {
        let pid = w.pid as i32;
        apps::bring_front(pid, w.id as u32);
        if let Ok(el) = self.element(w) {
            let _ = el.set_bool_attr("AXMain", true);
            let _ = el.perform("AXRaise");
        }
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_split() {
        assert_eq!(split_args(r#"-a "b c" d"#), vec!["-a", "b c", "d"]);
        assert_eq!(split_args("  "), Vec::<String>::new());
        assert_eq!(split_args(r#""""#), vec![""]);
    }

    #[test]
    fn open_arguments() {
        let req = LaunchRequest {
            name: "TextEdit".into(),
            bundle_id: "com.apple.TextEdit".into(),
            path: "/System/Applications/TextEdit.app".into(),
            open: Some("/tmp/x.txt".into()),
            args: String::new(),
            new_instance: false,
            as_admin: false,
        };
        assert_eq!(open_args(&req, |_| true), vec!["-g", "-a", "/System/Applications/TextEdit.app", "/tmp/x.txt"]);
        // app moved: by bundle id; missing document dropped
        assert_eq!(open_args(&req, |_| false), vec!["-g", "-b", "com.apple.TextEdit"]);
        let req = LaunchRequest { open: Some("https://example.com".into()), new_instance: true, args: "--foo bar".into(), ..req };
        assert_eq!(
            open_args(&req, |_| true),
            vec!["-g", "-n", "-a", "/System/Applications/TextEdit.app", "https://example.com", "--args", "--foo", "bar"]
        );
    }
}
