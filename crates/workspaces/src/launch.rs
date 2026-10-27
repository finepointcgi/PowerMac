//! Restoring a workspace (PowerToys' launcher + window arranger in one
//! loop).
//!
//! 1. Map every saved frame onto the current monitors
//!    ([`crate::geometry::map_frame`]).
//! 2. Apps that reuse existing windows: pair running windows with entries,
//!    nearest first (PowerToys' "move existing windows" phase), and place
//!    them.
//! 3. Launch the rest in order — each app after its delay, a second
//!    instance of the same app only once the previous one has its window
//!    (or after a few seconds, as PowerToys does for Outlook & co.) — then
//!    wait for new windows, give each to the oldest waiting entry of its app
//!    (documents must agree) and place it. Entries without a window after
//!    the timeout fail.
//! 4. Re-check placed windows for a moment: apps that restore their own
//!    frame after showing the window get put back (twice at most).
//! 5. Optionally minimize / hide other apps' windows (never quit), then
//!    bring the first placed window forward — the only activation.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::backend::{Backend, LaunchRequest, LiveWindow, Placement};
use crate::geometry::{self, MissingMonitor, Rect};
use crate::matching;
use crate::model::{App, OtherWindows, WindowState, Workspace};

/// Launch settings that aren't stored per workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LaunchOptions {
    /// How long to wait for an app's window (seconds).
    pub timeout_secs: u32,
    pub missing_monitor: MissingMonitor,
    /// Try to move windows back to their saved Space (private API; may not
    /// work on recent macOS).
    pub restore_spaces: bool,
    /// Bring the first app of the workspace forward at the end.
    pub activate_first: bool,
    /// Poll interval while waiting for windows (ms).
    pub poll_ms: u32,
    /// How long placed windows are watched for apps that move themselves
    /// back (ms).
    pub settle_ms: u32,
    /// Max wait for the previous instance of the same app (ms).
    pub same_app_gap_ms: u32,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            timeout_secs: 15,
            missing_monitor: MissingMonitor::ScaleToPrimary,
            restore_spaces: false,
            activate_first: true,
            poll_ms: 250,
            settle_ms: 1500,
            same_app_gap_ms: 3000,
        }
    }
}

/// Progress of one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppStatus {
    Pending,
    Launching,
    /// Launched; waiting for its window.
    Waiting,
    Placed,
    Skipped,
    Failed,
}

impl AppStatus {
    pub fn is_final(&self) -> bool {
        matches!(self, AppStatus::Placed | AppStatus::Skipped | AppStatus::Failed)
    }

    pub fn label(&self) -> &'static str {
        match self {
            AppStatus::Pending => "Pending",
            AppStatus::Launching => "Launching",
            AppStatus::Waiting => "Waiting for window",
            AppStatus::Placed => "Placed",
            AppStatus::Skipped => "Skipped",
            AppStatus::Failed => "Failed",
        }
    }
}

/// A progress update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchEvent {
    /// Index into the workspace's apps.
    pub index: usize,
    pub app: String,
    pub status: AppStatus,
    /// Failure reason, or a note ("moved existing window", "display
    /// missing: placed on the main display", …).
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LaunchReport {
    pub workspace: String,
    /// Final state of every entry (workspace order).
    pub results: Vec<LaunchEvent>,
    pub elapsed: Duration,
}

impl LaunchReport {
    pub fn all_ok(&self) -> bool {
        self.results.iter().all(|r| r.status != AppStatus::Failed)
    }

    pub fn summary(&self) -> String {
        let n = |s: AppStatus| self.results.iter().filter(|r| r.status == s).count();
        let mut s = format!("{}: {} placed", self.workspace, n(AppStatus::Placed));
        let sk = n(AppStatus::Skipped);
        if sk > 0 {
            s.push_str(&format!(", {sk} skipped"));
        }
        let f = n(AppStatus::Failed);
        if f > 0 {
            s.push_str(&format!(", {f} failed"));
        }
        s
    }
}

struct Entry<'a> {
    app: &'a App,
    key: String,
    target: Placement,
    fallback_note: Option<String>,
    status: AppStatus,
    detail: Option<String>,
    launched_at: Option<Duration>,
    window: Option<LiveWindow>,
}

struct Verify {
    index: usize,
    window: LiveWindow,
    frame: Rect,
    until: Duration,
    retries: u8,
}

fn ms(v: u32) -> Duration {
    Duration::from_millis(v as u64)
}

/// Restore `ws` with `backend`. Only per-entry problems are reported as
/// failures; an error is returned only when the window system can't be
/// read at all.
pub fn run(ws: &Workspace, backend: &dyn Backend, opts: &LaunchOptions, progress: &dyn Fn(&LaunchEvent)) -> Result<LaunchReport> {
    let start = backend.now();
    let own = backend.own_pid();
    let monitors = backend.monitors().unwrap_or_else(|e| {
        log::warn!("workspaces: reading monitors: {e:#}");
        vec![]
    });
    let before: Vec<LiveWindow> = backend.windows()?.into_iter().filter(|w| w.pid != own).collect();
    let before_ids: HashSet<u64> = before.iter().map(|w| w.id).collect();

    let mut entries: Vec<Entry> = ws
        .apps
        .iter()
        .map(|app| {
            let saved = ws.monitor(&app.monitor_id);
            let m = geometry::map_frame(&app.frame, saved, app.state == WindowState::Zoomed, &monitors, opts.missing_monitor);
            let state = if m.force_minimized { WindowState::Minimized } else { app.state };
            let fallback_note = m.fallback.then(|| {
                let name = saved.map(|s| s.name.as_str()).filter(|n| !n.is_empty()).unwrap_or("the saved display");
                if m.force_minimized {
                    format!("{name} is not connected: minimized on the main display")
                } else {
                    format!("{name} is not connected: placed on the main display")
                }
            });
            Entry {
                app,
                key: matching::app_key(app),
                target: Placement {
                    frame: m.frame,
                    state,
                    space: if opts.restore_spaces { app.space } else { None },
                    monitor_id: m.monitor_id,
                },
                fallback_note,
                status: AppStatus::Pending,
                detail: None,
                launched_at: None,
                window: None,
            }
        })
        .collect();

    let emit = |i: usize, e: &Entry| {
        progress(&LaunchEvent { index: i, app: e.app.name.clone(), status: e.status.clone(), detail: e.detail.clone() });
    };
    for (i, e) in entries.iter().enumerate() {
        emit(i, e);
    }

    let mut claimed: HashSet<u64> = HashSet::new();
    let mut verify: Vec<Verify> = Vec::new();

    let place = |e: &mut Entry, w: &LiveWindow, how: &str, verify: &mut Vec<Verify>, i: usize, now: Duration| {
        match backend.place(w, &e.target) {
            Ok(note) => {
                e.status = AppStatus::Placed;
                let notes: Vec<String> = [Some(how.to_string()).filter(|s| !s.is_empty()), e.fallback_note.clone(), note]
                    .into_iter()
                    .flatten()
                    .collect();
                e.detail = (!notes.is_empty()).then(|| notes.join("; "));
                if matches!(e.target.state, WindowState::Normal | WindowState::Zoomed) {
                    verify.push(Verify { index: i, window: w.clone(), frame: e.target.frame, until: now + ms(opts.settle_ms), retries: 0 });
                }
            }
            Err(err) => {
                e.status = AppStatus::Failed;
                e.detail = Some(format!("could not move the window: {err:#}"));
            }
        }
        e.window = Some(w.clone());
    };

    // Phase 1: reuse running windows.
    {
        let targets: Vec<(usize, &App, Rect)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| ws.reuses_existing(e.app))
            .map(|(i, e)| (i, e.app, e.target.frame))
            .collect();
        let pairs = matching::assign_existing(&targets, &before);
        let now = backend.now();
        for (i, wi) in pairs {
            let w = &before[wi];
            claimed.insert(w.id);
            place(&mut entries[i], w, "moved its open window", &mut verify, i, now);
            emit(i, &entries[i]);
        }
    }

    // Entries that would only activate a running app: nothing to launch.
    for (i, e) in entries.iter_mut().enumerate() {
        if e.status != AppStatus::Pending {
            continue;
        }
        let opens_something = e.app.document.is_some() || e.app.url.is_some() || e.app.options.launch_new_instance;
        let running = before.iter().any(|w| matching::same_app(e.app, &w.app));
        if !ws.reuses_existing(e.app) && running && !opens_something {
            e.status = AppStatus::Skipped;
            e.detail = Some("already running; its windows were left as they are".into());
            emit(i, e);
        }
    }

    // Phase 2: launch and wait.
    let mut last_launch: Option<Duration> = None;
    loop {
        let now = backend.now();

        // Launch whatever may go now (in order: a blocked entry blocks the
        // ones after it, so the workspace order is kept).
        for i in 0..entries.len() {
            if entries[i].status != AppStatus::Pending {
                continue;
            }
            let earliest = last_launch.unwrap_or(start) + ms(entries[i].app.options.delay_ms);
            if now < earliest {
                break;
            }
            let key = entries[i].key.clone();
            let busy = entries.iter().any(|o| {
                o.key == key && o.status == AppStatus::Waiting && o.launched_at.is_some_and(|t| now < t + ms(opts.same_app_gap_ms))
            });
            if busy {
                break;
            }
            let e = &mut entries[i];
            e.status = AppStatus::Launching;
            emit(i, e);
            let req = LaunchRequest {
                name: e.app.name.clone(),
                bundle_id: e.app.bundle_id.clone(),
                path: e.app.path.clone(),
                open: e.app.document.clone().or_else(|| e.app.url.clone()),
                args: e.app.command_line_args.clone(),
                new_instance: e.app.options.launch_new_instance,
                as_admin: e.app.launch_as_admin,
            };
            match backend.launch(&req) {
                Ok(()) => {
                    e.status = AppStatus::Waiting;
                    e.launched_at = Some(backend.now());
                    last_launch = e.launched_at;
                }
                Err(err) => {
                    e.status = AppStatus::Failed;
                    e.detail = Some(format!("could not launch: {err:#}"));
                    last_launch = Some(backend.now());
                }
            }
            emit(i, e);
        }

        let waiting = entries.iter().any(|e| e.status == AppStatus::Waiting);
        if waiting || !verify.is_empty() {
            let current = match backend.windows() {
                Ok(v) => v,
                Err(e) => {
                    log::debug!("workspaces: listing windows: {e:#}");
                    vec![]
                }
            };
            let now = backend.now();
            // New windows -> oldest waiting entry of the same app.
            let fresh: Vec<&LiveWindow> =
                current.iter().filter(|w| w.pid != own && !before_ids.contains(&w.id) && !claimed.contains(&w.id)).collect();
            for w in fresh {
                if w.frame.is_empty() && w.state != WindowState::Minimized {
                    continue;
                }
                let pick = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.status == AppStatus::Waiting && matching::same_app(e.app, &w.app))
                    .filter(|(_, e)| match (&e.app.document, &w.document) {
                        (Some(a), Some(b)) => a == b,
                        _ => true,
                    })
                    .min_by_key(|(_, e)| {
                        let doc_match = matches!((&e.app.document, &w.document), (Some(a), Some(b)) if a == b);
                        (!doc_match, e.launched_at)
                    })
                    .map(|(i, _)| i);
                if let Some(i) = pick {
                    claimed.insert(w.id);
                    place(&mut entries[i], w, "", &mut verify, i, now);
                    emit(i, &entries[i]);
                }
            }
            // Re-apply frames that the app changed back.
            verify.retain_mut(|v| {
                if now > v.until {
                    return false;
                }
                if let Some(f) = backend.window_frame(&v.window) {
                    if !f.approx_eq(&v.frame, 4) {
                        if v.retries >= 2 {
                            return false;
                        }
                        v.retries += 1;
                        let _ = backend.place(&v.window, &entries[v.index].target);
                    }
                    true
                } else {
                    false
                }
            });
            // Timeouts.
            for (i, e) in entries.iter_mut().enumerate() {
                if e.status == AppStatus::Waiting && e.launched_at.is_some_and(|t| now >= t + Duration::from_secs(opts.timeout_secs as u64)) {
                    e.status = AppStatus::Failed;
                    e.detail = Some(format!("no window appeared within {} s", opts.timeout_secs));
                    emit(i, e);
                }
            }
        }

        let pending = entries.iter().any(|e| matches!(e.status, AppStatus::Pending | AppStatus::Launching | AppStatus::Waiting));
        if !pending && verify.is_empty() {
            break;
        }
        // Sleep until the next poll, or the next delayed launch if sooner.
        let mut step = ms(opts.poll_ms.max(10));
        if !entries.iter().any(|e| e.status == AppStatus::Waiting) && verify.is_empty() {
            if let Some(next) = entries.iter().find(|e| e.status == AppStatus::Pending) {
                let due = last_launch.unwrap_or(start) + ms(next.app.options.delay_ms);
                let now = backend.now();
                if due > now {
                    step = due - now;
                }
            }
        }
        backend.sleep(step);
    }

    // Phase 3: other apps' windows.
    if ws.other_windows != OtherWindows::Leave {
        let ours: HashSet<u64> = entries.iter().filter_map(|e| e.window.as_ref().map(|w| w.id)).collect();
        let all = backend.windows().unwrap_or_default();
        let others: Vec<&LiveWindow> = all
            .iter()
            .filter(|w| w.pid != own && !ours.contains(&w.id) && !ws.apps.iter().any(|a| matching::same_app(a, &w.app)))
            .collect();
        match ws.other_windows {
            OtherWindows::Minimize => {
                for w in others.iter().filter(|w| w.state == WindowState::Normal || w.state == WindowState::Zoomed) {
                    if !w.app_hidden {
                        if let Err(e) = backend.minimize(w) {
                            log::debug!("workspaces: minimizing {}: {e:#}", w.app.name);
                        }
                    }
                }
            }
            OtherWindows::Hide => {
                let mut pids: Vec<i64> = others.iter().filter(|w| !w.app_hidden).map(|w| w.pid).collect();
                pids.sort();
                pids.dedup();
                for p in pids {
                    if let Err(e) = backend.hide_app(p) {
                        log::debug!("workspaces: hiding pid {p}: {e:#}");
                    }
                }
            }
            OtherWindows::Leave => {}
        }
    }

    // Phase 4: one activation.
    if opts.activate_first {
        if let Some(w) = entries
            .iter()
            .find(|e| e.status == AppStatus::Placed && e.target.state != WindowState::Minimized)
            .and_then(|e| e.window.as_ref())
        {
            backend.activate(w);
        }
    }

    let results = entries
        .iter()
        .enumerate()
        .map(|(i, e)| LaunchEvent { index: i, app: e.app.name.clone(), status: e.status.clone(), detail: e.detail.clone() })
        .collect();
    Ok(LaunchReport { workspace: ws.name.clone(), results, elapsed: backend.now().saturating_sub(start) })
}

#[cfg(test)]
pub(crate) mod fake {
    //! A scriptable window system with a virtual clock.

    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::time::Duration;

    use anyhow::{bail, Result};

    use crate::backend::*;
    use crate::geometry::{Monitor, Rect};
    use crate::model::WindowState;

    /// What happens when an app (by bundle id) is launched.
    #[derive(Clone)]
    pub enum Script {
        /// A window appears after `after` (frame as the app chooses).
        Appear { after: Duration, frame: Rect, document: Option<String> },
        Fail(String),
        Never,
    }

    #[derive(Default)]
    pub struct Fake {
        pub t: Cell<Duration>,
        pub monitors: Vec<Monitor>,
        pub windows: RefCell<Vec<LiveWindow>>,
        pub scripts: RefCell<HashMap<String, Vec<Script>>>,
        pending: RefCell<Vec<(Duration, LiveWindow)>>,
        pub launches: RefCell<Vec<(Duration, LaunchRequest)>>,
        pub placed: RefCell<Vec<(u64, Placement)>>,
        pub minimized: RefCell<Vec<u64>>,
        pub hidden: RefCell<Vec<i64>>,
        pub activated: RefCell<Vec<u64>>,
        /// Windows that jump back to this frame once after being placed.
        pub snap_back: RefCell<HashMap<u64, Rect>>,
        next_id: Cell<u64>,
    }

    pub fn ident(bundle: &str) -> AppIdentity {
        AppIdentity { bundle_id: bundle.into(), path: format!("/Applications/{bundle}.app"), name: bundle.into() }
    }

    impl Fake {
        pub fn new(monitors: Vec<Monitor>) -> Self {
            let f = Fake { monitors, ..Default::default() };
            f.next_id.set(100);
            f
        }

        pub fn add_window(&self, bundle: &str, pid: i64, frame: Rect, state: WindowState, document: Option<&str>) -> u64 {
            let id = self.next_id.get();
            self.next_id.set(id + 1);
            self.windows.borrow_mut().push(LiveWindow {
                id,
                pid,
                app: ident(bundle),
                title: format!("{bundle} window"),
                frame,
                state,
                space: Some(1),
                document: document.map(String::from),
                url: None,
                app_hidden: false,
                elevated: false,
            });
            id
        }

        pub fn script(&self, bundle: &str, s: Script) {
            self.scripts.borrow_mut().entry(bundle.into()).or_default().push(s);
        }

        pub fn window(&self, id: u64) -> LiveWindow {
            self.windows.borrow().iter().find(|w| w.id == id).cloned().unwrap()
        }

        fn pid_for(bundle: &str) -> i64 {
            1000 + bundle.bytes().map(|b| b as i64).sum::<i64>()
        }
    }

    impl Backend for Fake {
        fn monitors(&self) -> Result<Vec<Monitor>> {
            Ok(self.monitors.clone())
        }

        fn windows(&self) -> Result<Vec<LiveWindow>> {
            let now = self.t.get();
            let mut p = self.pending.borrow_mut();
            let (due, later): (Vec<_>, Vec<_>) = p.drain(..).partition(|(t, _)| *t <= now);
            *p = later;
            self.windows.borrow_mut().extend(due.into_iter().map(|(_, w)| w));
            Ok(self.windows.borrow().clone())
        }

        fn launch(&self, req: &LaunchRequest) -> Result<()> {
            let now = self.t.get();
            self.launches.borrow_mut().push((now, req.clone()));
            let s = {
                let mut scripts = self.scripts.borrow_mut();
                let v = scripts.entry(req.bundle_id.clone()).or_default();
                if v.len() > 1 {
                    v.remove(0)
                } else {
                    v.first().cloned().unwrap_or(Script::Never)
                }
            };
            match s {
                Script::Fail(m) => bail!(m),
                Script::Never => Ok(()),
                Script::Appear { after, frame, document } => {
                    let id = self.next_id.get();
                    self.next_id.set(id + 1);
                    let w = LiveWindow {
                        id,
                        pid: Self::pid_for(&req.bundle_id),
                        app: ident(&req.bundle_id),
                        title: "new".into(),
                        frame,
                        state: WindowState::Normal,
                        space: Some(1),
                        document: document.or_else(|| req.open.clone()),
                        url: None,
                        app_hidden: false,
                        elevated: false,
                    };
                    self.pending.borrow_mut().push((now + after, w));
                    Ok(())
                }
            }
        }

        fn place(&self, w: &LiveWindow, p: &Placement) -> Result<Option<String>> {
            self.placed.borrow_mut().push((w.id, p.clone()));
            let mut ws = self.windows.borrow_mut();
            let Some(x) = ws.iter_mut().find(|x| x.id == w.id) else { bail!("window is gone") };
            x.frame = p.frame;
            x.state = p.state;
            Ok(None)
        }

        fn window_frame(&self, w: &LiveWindow) -> Option<Rect> {
            let mut ws = self.windows.borrow_mut();
            let x = ws.iter_mut().find(|x| x.id == w.id)?;
            if let Some(r) = self.snap_back.borrow_mut().remove(&w.id) {
                x.frame = r;
            }
            Some(x.frame)
        }

        fn minimize(&self, w: &LiveWindow) -> Result<()> {
            self.minimized.borrow_mut().push(w.id);
            Ok(())
        }

        fn hide_app(&self, pid: i64) -> Result<()> {
            self.hidden.borrow_mut().push(pid);
            Ok(())
        }

        fn activate(&self, w: &LiveWindow) {
            self.activated.borrow_mut().push(w.id);
        }

        fn own_pid(&self) -> i64 {
            1
        }

        fn now(&self) -> Duration {
            self.t.get()
        }

        fn sleep(&self, d: Duration) {
            self.t.set(self.t.get() + d);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::Duration;

    use super::fake::{Fake, Script};
    use super::*;
    use crate::geometry::Monitor;
    use crate::model::{AppOptions, ExistingWindows};

    fn monitors() -> Vec<Monitor> {
        vec![
            Monitor { id: "A".into(), name: "Built-in".into(), number: 0, frame: Rect::new(0, 0, 1512, 982), work_area: Rect::new(0, 33, 1512, 949), scale: 2.0, primary: true },
            Monitor { id: "B".into(), name: "Studio".into(), number: 1, frame: Rect::new(1512, 0, 2560, 1440), work_area: Rect::new(1512, 25, 2560, 1415), scale: 2.0, primary: false },
        ]
    }

    fn app(bundle: &str, frame: Rect, monitor: &str) -> App {
        App {
            id: crate::store::new_id(),
            name: bundle.into(),
            bundle_id: bundle.into(),
            path: format!("/Applications/{bundle}.app"),
            frame,
            monitor_id: monitor.into(),
            ..Default::default()
        }
    }

    fn ws(apps: Vec<App>) -> Workspace {
        Workspace { id: "w".into(), name: "Test".into(), monitors: monitors(), apps, move_existing_windows: true, ..Default::default() }
    }

    fn run_collect(w: &Workspace, f: &Fake, opts: &LaunchOptions) -> (LaunchReport, Vec<LaunchEvent>) {
        let log = RefCell::new(Vec::new());
        let r = run(w, f, opts, &|e| log.borrow_mut().push(e.clone())).unwrap();
        (r, log.into_inner())
    }

    fn statuses(r: &LaunchReport) -> Vec<AppStatus> {
        r.results.iter().map(|e| e.status.clone()).collect()
    }

    #[test]
    fn launches_late_apps_and_places_them() {
        let f = Fake::new(monitors());
        f.script("ed", Script::Appear { after: Duration::from_millis(2600), frame: Rect::new(10, 40, 300, 200), document: None });
        f.script("calc", Script::Appear { after: Duration::from_millis(400), frame: Rect::new(10, 40, 300, 200), document: None });
        let w = ws(vec![app("ed", Rect::new(1600, 100, 1000, 800), "B"), app("calc", Rect::new(100, 100, 230, 400), "A")]);
        let (r, events) = run_collect(&w, &f, &LaunchOptions::default());
        assert_eq!(statuses(&r), vec![AppStatus::Placed, AppStatus::Placed]);
        // both launched right away (different apps), in order
        let l = f.launches.borrow();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].1.bundle_id, "ed");
        assert_eq!(l[0].0, l[1].0);
        // frames applied
        let placed = f.placed.borrow();
        assert!(placed.iter().any(|(_, p)| p.frame == Rect::new(1600, 100, 1000, 800)));
        assert!(placed.iter().any(|(_, p)| p.frame == Rect::new(100, 100, 230, 400)));
        // progress went through the states for "ed"
        let ed: Vec<AppStatus> = events.iter().filter(|e| e.index == 0).map(|e| e.status.clone()).collect();
        assert_eq!(ed, vec![AppStatus::Pending, AppStatus::Launching, AppStatus::Waiting, AppStatus::Placed]);
        // one activation, the first app
        assert_eq!(f.activated.borrow().len(), 1);
        let ed_window = f.placed.borrow().iter().find(|(_, p)| p.frame.x == 1600).unwrap().0;
        assert_eq!(f.activated.borrow()[0], ed_window);
    }

    #[test]
    fn failures_and_timeouts_are_reported() {
        let f = Fake::new(monitors());
        f.script("broken", Script::Fail("no such app".into()));
        f.script("slow", Script::Never);
        f.script("ok", Script::Appear { after: Duration::from_millis(300), frame: Rect::new(0, 40, 100, 100), document: None });
        let w = ws(vec![app("broken", Rect::new(0, 40, 500, 500), "A"), app("slow", Rect::new(0, 40, 500, 500), "A"), app("ok", Rect::new(0, 40, 500, 500), "A")]);
        let opts = LaunchOptions { timeout_secs: 5, ..Default::default() };
        let (r, _) = run_collect(&w, &f, &opts);
        assert_eq!(statuses(&r), vec![AppStatus::Failed, AppStatus::Failed, AppStatus::Placed]);
        assert!(r.results[0].detail.as_deref().unwrap().contains("no such app"));
        assert!(r.results[1].detail.as_deref().unwrap().contains("within 5 s"));
        assert!(!r.all_ok());
        assert!(r.elapsed >= Duration::from_secs(5) && r.elapsed < Duration::from_secs(7), "{:?}", r.elapsed);
        assert_eq!(r.summary(), "Test: 1 placed, 2 failed");
    }

    #[test]
    fn existing_windows_are_moved_not_relaunched() {
        let f = Fake::new(monitors());
        let near = f.add_window("ed", 50, Rect::new(1620, 120, 990, 790), WindowState::Normal, None);
        let far = f.add_window("ed", 50, Rect::new(0, 40, 400, 300), WindowState::Normal, None);
        let w = ws(vec![app("ed", Rect::new(1600, 100, 1000, 800), "B")]);
        let (r, _) = run_collect(&w, &f, &LaunchOptions::default());
        assert_eq!(statuses(&r), vec![AppStatus::Placed]);
        assert!(f.launches.borrow().is_empty());
        assert_eq!(f.placed.borrow()[0].0, near);
        assert_ne!(f.placed.borrow()[0].0, far);
        assert!(r.results[0].detail.as_deref().unwrap().contains("moved"));
    }

    #[test]
    fn leave_mode_skips_running_apps_without_documents() {
        let f = Fake::new(monitors());
        f.add_window("ed", 50, Rect::new(0, 40, 400, 300), WindowState::Normal, None);
        let mut a = app("ed", Rect::new(1600, 100, 1000, 800), "B");
        a.options.existing_windows = ExistingWindows::Leave;
        let mut b = app("ed", Rect::new(100, 100, 600, 600), "A");
        b.options.existing_windows = ExistingWindows::Leave;
        b.document = Some("/tmp/x.txt".into());
        f.script("ed", Script::Appear { after: Duration::from_millis(500), frame: Rect::new(0, 40, 400, 300), document: None });
        let w = ws(vec![a, b]);
        let (r, _) = run_collect(&w, &f, &LaunchOptions::default());
        assert_eq!(statuses(&r), vec![AppStatus::Skipped, AppStatus::Placed]);
        let l = f.launches.borrow();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].1.open.as_deref(), Some("/tmp/x.txt"));
    }

    #[test]
    fn same_app_instances_wait_for_each_other_and_documents_route() {
        let f = Fake::new(monitors());
        // second document's window shows up first
        f.script("ed", Script::Appear { after: Duration::from_millis(1200), frame: Rect::new(0, 40, 100, 100), document: None });
        f.script("ed", Script::Appear { after: Duration::from_millis(200), frame: Rect::new(0, 40, 100, 100), document: None });
        let mut a = app("ed", Rect::new(0, 40, 700, 900), "A");
        a.document = Some("/tmp/a.txt".into());
        let mut b = app("ed", Rect::new(760, 40, 700, 900), "A");
        b.document = Some("/tmp/b.txt".into());
        let w = ws(vec![a, b]);
        let (r, _) = run_collect(&w, &f, &LaunchOptions::default());
        assert_eq!(statuses(&r), vec![AppStatus::Placed, AppStatus::Placed]);
        let l = f.launches.borrow();
        // b launched only after a's window appeared (1.2 s), not right away
        assert!(l[1].0 >= Duration::from_millis(1200), "{:?}", l[1].0);
        // each document's window got its own frame
        for w in f.windows.borrow().iter() {
            match w.document.as_deref() {
                Some("/tmp/a.txt") => assert_eq!(w.frame.x, 0),
                Some("/tmp/b.txt") => assert_eq!(w.frame.x, 760),
                _ => panic!("unexpected window"),
            }
        }
    }

    #[test]
    fn same_app_gap_is_bounded() {
        let f = Fake::new(monitors());
        f.script("ed", Script::Never);
        let w = ws(vec![app("ed", Rect::new(0, 40, 100, 100), "A"), app("ed", Rect::new(0, 40, 100, 100), "A")]);
        let mut w = w;
        for a in &mut w.apps {
            a.options.launch_new_instance = true;
        }
        let opts = LaunchOptions { timeout_secs: 10, same_app_gap_ms: 3000, ..Default::default() };
        let (r, _) = run_collect(&w, &f, &opts);
        assert_eq!(statuses(&r), vec![AppStatus::Failed, AppStatus::Failed]);
        let l = f.launches.borrow();
        assert!(l[1].0 >= Duration::from_secs(3) && l[1].0 < Duration::from_millis(3500), "{:?}", l[1].0);
        assert!(l.iter().all(|(_, r)| r.new_instance));
    }

    #[test]
    fn delays_are_honoured() {
        let f = Fake::new(monitors());
        f.script("a", Script::Appear { after: Duration::from_millis(100), frame: Rect::new(0, 40, 100, 100), document: None });
        f.script("b", Script::Appear { after: Duration::from_millis(100), frame: Rect::new(0, 40, 100, 100), document: None });
        let mut b = app("b", Rect::new(0, 40, 300, 300), "A");
        b.options = AppOptions { delay_ms: 2000, ..Default::default() };
        let w = ws(vec![app("a", Rect::new(0, 40, 300, 300), "A"), b]);
        let (r, _) = run_collect(&w, &f, &LaunchOptions::default());
        assert_eq!(statuses(&r), vec![AppStatus::Placed, AppStatus::Placed]);
        let l = f.launches.borrow();
        assert_eq!(l[0].0, Duration::ZERO);
        assert_eq!(l[1].0, Duration::from_millis(2000));
    }

    #[test]
    fn missing_monitor_falls_back_and_says_so() {
        let mut mons = monitors();
        mons.truncate(1);
        let f = Fake::new(mons);
        f.script("ed", Script::Appear { after: Duration::from_millis(100), frame: Rect::new(0, 40, 100, 100), document: None });
        let w = ws(vec![app("ed", Rect::new(1512, 25, 1280, 1415), "B")]);
        let (r, _) = run_collect(&w, &f, &LaunchOptions::default());
        assert_eq!(r.results[0].status, AppStatus::Placed);
        assert!(r.results[0].detail.as_deref().unwrap().contains("Studio is not connected"));
        assert_eq!(f.placed.borrow()[0].1.frame, Rect::new(0, 33, 756, 949));

        let f = Fake::new(vec![monitors().remove(0)]);
        f.script("ed", Script::Appear { after: Duration::from_millis(100), frame: Rect::new(0, 40, 100, 100), document: None });
        let opts = LaunchOptions { missing_monitor: MissingMonitor::PrimaryMinimized, ..Default::default() };
        let (_, _) = run_collect(&w, &f, &opts);
        assert_eq!(f.placed.borrow()[0].1.state, WindowState::Minimized);
        assert!(f.activated.borrow().is_empty(), "minimized windows aren't activated");
    }

    #[test]
    fn windows_that_snap_back_are_placed_again() {
        let f = Fake::new(monitors());
        let id = f.add_window("ed", 50, Rect::new(0, 40, 400, 300), WindowState::Normal, None);
        f.snap_back.borrow_mut().insert(id, Rect::new(0, 40, 400, 300));
        let w = ws(vec![app("ed", Rect::new(1600, 100, 1000, 800), "B")]);
        let (r, _) = run_collect(&w, &f, &LaunchOptions::default());
        assert_eq!(r.results[0].status, AppStatus::Placed);
        assert_eq!(f.placed.borrow().len(), 2);
        assert_eq!(f.window(id).frame, Rect::new(1600, 100, 1000, 800));
    }

    #[test]
    fn other_windows_minimized_or_hidden_never_ours() {
        for mode in [OtherWindows::Minimize, OtherWindows::Hide] {
            let f = Fake::new(monitors());
            let mine = f.add_window("ed", 50, Rect::new(0, 40, 400, 300), WindowState::Normal, None);
            let other = f.add_window("mail", 60, Rect::new(0, 40, 400, 300), WindowState::Normal, None);
            let other_min = f.add_window("mail", 60, Rect::new(0, 40, 400, 300), WindowState::Minimized, None);
            let own = f.add_window("powermac", 1, Rect::new(0, 40, 400, 300), WindowState::Normal, None);
            let mut w = ws(vec![app("ed", Rect::new(1600, 100, 1000, 800), "B")]);
            w.other_windows = mode;
            run_collect(&w, &f, &LaunchOptions::default());
            match mode {
                OtherWindows::Minimize => {
                    assert_eq!(*f.minimized.borrow(), vec![other]);
                    assert!(f.hidden.borrow().is_empty());
                }
                _ => {
                    assert_eq!(*f.hidden.borrow(), vec![60]);
                    assert!(f.minimized.borrow().is_empty());
                }
            }
            let _ = (mine, other_min, own);
        }
    }

    #[test]
    fn own_windows_are_ignored() {
        let f = Fake::new(monitors());
        f.add_window("ed", 1, Rect::new(0, 40, 400, 300), WindowState::Normal, None);
        f.script("ed", Script::Never);
        let w = ws(vec![app("ed", Rect::new(1600, 100, 1000, 800), "B")]);
        let opts = LaunchOptions { timeout_secs: 1, ..Default::default() };
        let (r, _) = run_collect(&w, &f, &opts);
        // not reused although it is an "ed" window: it is ours (pid 1)
        assert_eq!(r.results[0].status, AppStatus::Failed);
        assert_eq!(f.launches.borrow().len(), 1);
    }
}
