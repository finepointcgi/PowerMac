//! The app talking about itself to the settings window, and the app-wide
//! chores that belong to no module:
//!
//! * `app-status.json` every 2 s: permissions as this process sees them,
//!   the shared input hook's health (`pm_hook`), one line per module
//!   (running / waiting for permission / failed / off).
//! * `app-request.json` (polled every 250 ms): restart, launch / capture a
//!   workspace, open the Workspaces editor, check for updates.
//! * The daily update check (GitHub Releases, `pm_update`) and its result in
//!   `update-status.json`.
//! * Restarting: a clean shutdown, then `open -n` of the bundle (or this
//!   executable) with `--wait-pid`, so the new instance starts only after
//!   this one is gone, and with `--open-settings` to bring the settings
//!   window back.
//! * The Workspaces hotkey (a `pm_hook` subscription).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pm_config::bridge::{self, AppPermissions, AppRequest, AppStatus, HookHealth, ModuleState, ModuleStatus, SlowHandler};
use pm_config::AppConfig;
use settings_ui::permissions::{self as perms, Perm, Probe};

use crate::{Running, RUNNING};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

fn now_secs() -> u64 {
    pm_update::now_secs()
}

static STARTED_AT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Start the status / request thread (once).
pub fn start_once() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = STARTED_AT.set(now_secs());
    let Some(path) = config_path() else { return };
    std::thread::Builder::new()
        .name("app-glue".into())
        .spawn(move || {
            let mut requests = bridge::RequestReader::new(&path, bridge::APP_REQUEST_FILE);
            let mut last_status: Option<Instant> = None;
            let started = Instant::now();
            let mut next_update_try = Instant::now() + Duration::from_secs(30);
            loop {
                if let Some(req) = requests.poll::<AppRequest>(&path, bridge::APP_REQUEST_FILE) {
                    handle_request(&path, req);
                }
                if last_status.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
                    last_status = Some(Instant::now());
                    if let Some(st) = status() {
                        if let Err(e) = bridge::write(&path, bridge::APP_STATUS_FILE, &st) {
                            log::debug!("app status file: {e:#}");
                        }
                    }
                }
                if started.elapsed() >= Duration::from_secs(30) && Instant::now() >= next_update_try {
                    next_update_try = Instant::now() + Duration::from_secs(60);
                    let cfg = crate::current_config();
                    if cfg.updates.check && pm_update::due(cfg.updates.last_check, now_secs()) {
                        if let Some(retry) = check_updates(&path, &cfg, false) {
                            next_update_try = Instant::now() + retry;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        })
        .expect("spawn app-glue");
}

fn handle_request(path: &Path, req: AppRequest) {
    log::info!("settings asked: {req:?}");
    match req {
        AppRequest::Restart { reopen_page } => restart(reopen_page.as_deref()),
        AppRequest::LaunchWorkspace { id } => launch_workspace(id),
        AppRequest::CaptureWorkspace { name } => capture_workspace(name),
        AppRequest::OpenWorkspacesEditor => open_workspaces_editor(),
        AppRequest::CheckUpdates => {
            let cfg = crate::current_config();
            let path = path.to_path_buf();
            std::thread::spawn(move || {
                check_updates(&path, &cfg, true);
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Status

fn tap_state(s: &pm_hook::TapState) -> (&'static str, String) {
    use pm_hook::TapState as T;
    match s {
        T::Idle => ("idle", String::new()),
        T::Starting => ("starting", String::new()),
        T::Active => ("active", String::new()),
        T::ListenOnly { keyboard } => (
            "listenOnly",
            if *keyboard { "without Accessibility: sees input but can't change it".into() } else { "without Accessibility and Input Monitoring: no keys".into() },
        ),
        T::Disabled => ("disabled", "macOS turned the tap off and it couldn't be turned back on".into()),
        T::Unavailable(why) => ("unavailable", why.clone()),
    }
}

/// The shared input hook's health.
pub(crate) fn hook_health() -> HookHealth {
    let st = pm_hook::status();
    let (state, detail) = tap_state(&st.state);
    let healthy = st.state == pm_hook::TapState::Active && pm_hook::is_healthy();
    let slow_handlers = pm_hook::stats()
        .into_iter()
        .map(|s| SlowHandler { name: s.name, slow: s.slow, calls: s.calls, max_ms: s.max.as_millis() as u64 })
        .collect();
    HookHealth {
        state: state.into(),
        detail,
        healthy,
        secure_input: st.secure_input,
        reenabled: st.reenabled,
        subscribers: pm_hook::subscribers().into_iter().map(|(n, _)| n).collect(),
        slow_handlers,
    }
}

/// What one module needs and whether it runs.
#[derive(Debug, Clone)]
pub(crate) struct ModuleRow {
    pub page: &'static str,
    pub name: &'static str,
    pub enabled: bool,
    pub running: bool,
    /// Core permissions it can't work without.
    pub needs: &'static [Perm],
    pub detail: String,
}

/// The status lines for `rows`, given this process' permissions.
pub(crate) fn module_statuses(rows: &[ModuleRow], probe: &Probe) -> Vec<ModuleStatus> {
    rows.iter()
        .map(|r| {
            let missing: Vec<&str> = if cfg!(target_os = "macos") {
                r.needs.iter().filter(|p| !probe.get(**p).granted()).map(|p| p.title()).collect()
            } else {
                Vec::new()
            };
            let (state, detail) = if !r.enabled {
                (ModuleState::Off, String::new())
            } else if !missing.is_empty() {
                (ModuleState::WaitingForPermission, format!("needs {}", missing.join(" and ")))
            } else if !r.running {
                (ModuleState::Failed, String::new())
            } else {
                (ModuleState::Running, r.detail.clone())
            };
            ModuleStatus { page: r.page.into(), name: r.name.into(), state, detail }
        })
        .collect()
}

fn module_rows(c: &AppConfig, r: &Running, ws_count: usize) -> Vec<ModuleRow> {
    use Perm::*;
    let km = &c.keyboard_manager;
    let mu = &c.mouse_utils;
    let row = |page, name, enabled, running, needs: &'static [Perm], detail: String| ModuleRow { page, name, enabled, running, needs, detail };
    let awake_active = r.awake.as_ref().is_some_and(|a| a.status().active);
    vec![
        row("fancyzones", "FancyZones", c.fancyzones.enabled, r.zones.is_some(), &[Accessibility], String::new()),
        row("window-switcher", "Window Switcher", c.window_switcher.enabled, r.switcher.is_some(), &[Accessibility], c.window_switcher.trigger_text()),
        row("workspaces", "Workspaces", c.workspaces.enabled, true, &[Accessibility], format!("{ws_count} saved")),
        row("notch-shelf", "Notch Shelf", c.notch_shelf.enabled, r.shelf.is_some(), &[], String::new()),
        row("dock-previews", "Dock Previews", c.dock_previews.enabled && dock_previews::SUPPORTED, r.dock.is_some(), &[Accessibility], String::new()),
        row("menu-bar", "Menu Bar", c.menu_bar.enabled, r.menubar.is_some(), &[], String::new()),
        row("mouse", "Mouse", c.mouse_tuning.enabled, r.mouse_tuning.is_some(), &[], String::new()),
        row("scrolling", "Scrolling", c.scroll_reverser.enabled, r.scroll.is_some(), &[Accessibility], String::new()),
        row(
            "mouse-utilities",
            "Mouse Utilities",
            mu.find_my_mouse.enabled || mu.highlighter.enabled || mu.crosshairs.enabled,
            r.mouse.is_some(),
            &[],
            String::new(),
        ),
        row(
            "keyboard-manager",
            "Keyboard Manager",
            km.enabled || km.paste_plain_text.enabled || km.hyper_key.is_on(),
            r.keyboard.is_some(),
            &[Accessibility],
            String::new(),
        ),
        row("text-expansion", "Text Expansion", c.text_expander.enabled, r.expander.is_some(), &[Accessibility], String::new()),
        row(
            "displays",
            "Displays",
            c.display_control.enabled,
            r.display.is_some(),
            &[],
            r.display.as_ref().map(|d| plural(d.displays().len(), "display")).unwrap_or_default(),
        ),
        row("sound", "Sound", c.audio.enabled, r.audio.is_some(), &[], String::new()),
        row("awake", "Awake", c.awake.enabled, r.awake.is_some(), &[], if awake_active { "keeping the Mac awake".into() } else { "idle".into() }),
        row("clipboard-history", "Clipboard History", c.clipboard_history.enabled, r.clipboard.is_some(), &[], String::new()),
        row("color-picker", "Color Picker", c.color_picker.enabled, r.color.is_some(), &[], String::new()),
        row("text-extractor", "Text Extractor", c.text_extractor.enabled, r.ocr.is_some(), &[ScreenRecording], String::new()),
        row(
            "mouse-without-borders",
            "Mouse Without Borders",
            c.mwb.enabled,
            r.mwb.is_some(),
            &[Accessibility, InputMonitoring],
            r.mwb.as_ref().map(|m| crate::mwb_glue::status_line(&m.status())).unwrap_or_default(),
        ),
    ]
}

fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

trait TriggerText {
    fn trigger_text(&self) -> String;
}

impl TriggerText for window_switcher::Config {
    fn trigger_text(&self) -> String {
        if self.replace_cmd_tab {
            "replaces Cmd+Tab".into()
        } else {
            self.trigger.to_string()
        }
    }
}

/// The status file contents (`None` while the modules are being reconfigured).
fn status() -> Option<AppStatus> {
    let cfg = crate::current_config();
    let probe = perms::probe_in_process();
    let ws_count = workspaces::list(&data_dir()).map(|l| l.len()).unwrap_or(0);
    let rows = match RUNNING.try_lock() {
        Ok(g) => g.as_ref().map(|r| module_rows(&cfg, r, ws_count))?,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner().as_ref().map(|r| module_rows(&cfg, r, ws_count))?,
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    Some(AppStatus {
        version: VERSION.into(),
        pid: std::process::id(),
        started_at: STARTED_AT.get().copied().unwrap_or(0),
        executable: std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_default(),
        permissions: AppPermissions {
            accessibility: probe.accessibility.granted(),
            input_monitoring: probe.input_monitoring.granted(),
            screen_recording: probe.screen_recording.granted(),
        },
        hook: hook_health(),
        modules: module_statuses(&rows, &probe),
    })
}

/// The settings page to open at startup: the setup on first launch, its
/// Permissions step when an enabled module misses a permission (unless the
/// user turned that reminder off).
pub fn startup_page(cfg: &AppConfig) -> Option<&'static str> {
    if !cfg.setup.completed {
        return Some("welcome");
    }
    if cfg.setup.remind_missing_permissions && !perms::missing_needed(cfg, &perms::probe_in_process()).is_empty() {
        return Some("permissions");
    }
    None
}

/// Core permissions an enabled module needs that this process lacks (tray warning).
pub fn missing_permissions() -> Vec<Perm> {
    perms::missing_needed(&crate::current_config(), &perms::probe_in_process())
}

// ---------------------------------------------------------------------------
// Restart

/// The `.app` bundle this executable is in.
pub(crate) fn bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find(|a| a.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf)
}

/// The app runs without its tray icon (`run --no-tray`); a restart keeps that.
pub static NO_TRAY: AtomicBool = AtomicBool::new(false);

/// The arguments of the restarted instance.
pub(crate) fn restart_args(config: &Path, pid: u32, reopen: Option<&str>, no_tray: bool) -> Vec<String> {
    let mut v = vec!["--config".to_string(), config.display().to_string(), "run".into(), "--wait-pid".into(), pid.to_string()];
    if no_tray {
        v.push("--no-tray".into());
    }
    if let Some(p) = reopen {
        v.push("--open-settings".into());
        v.push(p.into());
    }
    v
}

/// Quit cleanly and start again (new permissions only reach a new process).
pub fn restart(reopen: Option<&str>) {
    let Ok(exe) = std::env::current_exe() else {
        log::error!("restart: no executable path");
        return;
    };
    let Some(config) = config_path() else { return };
    let args = restart_args(&config, std::process::id(), reopen, NO_TRAY.load(Ordering::Relaxed));
    log::info!("restarting PowerMac ({})", args.join(" "));
    crate::shutdown();
    let r = match bundle_of(&exe) {
        Some(b) if cfg!(target_os = "macos") => std::process::Command::new("/usr/bin/open").arg("-n").arg(&b).arg("--args").args(&args).status().map(|_| ()),
        _ => std::process::Command::new(&exe).args(&args).stdin(std::process::Stdio::null()).spawn().map(|_| ()),
    };
    if let Err(e) = r {
        log::error!("restart: starting the new instance failed: {e}");
    }
    std::process::exit(0);
}

/// `--wait-pid`: wait (up to 10 s) for the previous instance to exit.
pub fn wait_for_exit(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && process_alive(pid) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    // The old instance exits right after starting this one.
    std::thread::sleep(Duration::from_millis(1500));
    false
}

// ---------------------------------------------------------------------------
// Updates

static CHECKING: AtomicBool = AtomicBool::new(false);
/// The release the tray offers (refreshed from `update-status.json`).
static OFFER: Mutex<Option<(Instant, Option<pm_update::Release>)>> = Mutex::new(None);

/// Check GitHub now and record the result. Returns a shorter retry delay
/// when the check failed for a transient reason (offline).
pub(crate) fn check_updates(path: &Path, cfg: &AppConfig, manual: bool) -> Option<Duration> {
    if CHECKING.swap(true, Ordering::SeqCst) {
        return None;
    }
    let r = pm_update::check(&pm_update::CheckOptions::new(VERSION, cfg.updates.include_prereleases));
    let offline = matches!(r, Err(pm_update::CheckError::Offline { .. }));
    match &r {
        Ok(o) => log::info!("update check: {o:?}"),
        Err(e) => log::warn!("update check: {e}"),
    }
    let mut st: pm_update::UpdateState = bridge::read(path, bridge::UPDATE_STATUS_FILE).map(|s| s.data).unwrap_or_default();
    let was = st.available(VERSION).map(|r| r.version.clone());
    st.record(VERSION, r);
    if let Err(e) = bridge::write(path, bridge::UPDATE_STATUS_FILE, &st) {
        log::warn!("update status file: {e:#}");
    }
    if !offline {
        let now = now_secs();
        if let Err(e) = crate::edit_config(|c| c.updates.last_check = Some(now)) {
            log::warn!("update check: saving lastCheck: {e:#}");
        }
    }
    if let Some(rel) = st.available(VERSION) {
        if manual || was.as_deref() != Some(rel.version.as_str()) {
            crate::tray::notify("PowerMac update available", &format!("PowerMac {} is out (you have {VERSION}).", rel.version));
        }
    }
    *OFFER.lock().unwrap_or_else(|e| e.into_inner()) = None;
    CHECKING.store(false, Ordering::SeqCst);
    offline.then(|| Duration::from_secs(60 * 60))
}

/// The newer release to offer in the tray, if any (re-read every 10 s).
pub fn offered_update() -> Option<pm_update::Release> {
    let mut g = OFFER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, r)) = g.as_ref() {
        if at.elapsed() < Duration::from_secs(10) {
            return r.clone();
        }
    }
    let r = config_path()
        .and_then(|p| bridge::read::<pm_update::UpdateState>(&p, bridge::UPDATE_STATUS_FILE))
        .and_then(|s| s.data.available(VERSION).cloned());
    *g = Some((Instant::now(), r.clone()));
    r
}

// ---------------------------------------------------------------------------
// Workspaces

fn data_dir() -> PathBuf {
    config_path().map(|p| pm_config::data_dir_for(&p)).unwrap_or_else(|| PathBuf::from("."))
}

/// The saved workspaces (id, name).
pub fn workspace_list() -> Vec<(String, String)> {
    workspaces::list(&data_dir()).unwrap_or_default().into_iter().map(|w| (w.id, w.name)).collect()
}

/// When `workspaces.json` last changed (to rebuild the tray menu).
pub fn workspaces_stamp() -> Option<std::time::SystemTime> {
    workspaces::store::stamp(&data_dir())
}

fn workspaces_ready() -> Option<workspaces::Config> {
    let cfg = crate::current_config().workspaces;
    if !cfg.enabled {
        crate::tray::notify("Workspaces", "Workspaces is off (Settings → Workspaces).");
        return None;
    }
    if !workspaces::has_permissions() {
        crate::tray::notify("Workspaces", "PowerMac needs Accessibility to read and move windows (Settings → General).");
        return None;
    }
    Some(cfg)
}

/// Restore a workspace on another thread; a notification reports the result.
pub fn launch_workspace(id: String) {
    std::thread::Builder::new()
        .name("workspace-launch".into())
        .spawn(move || {
            let Some(cfg) = workspaces_ready() else { return };
            match workspaces::launch(&data_dir(), &id, &cfg, &|ev| log::debug!("workspace: {} {:?}", ev.app, ev.status)) {
                Ok(report) => {
                    let s = report.summary();
                    log::info!("workspace launched: {s} ({:.1} s)", report.elapsed.as_secs_f32());
                    crate::tray::notify("Workspace restored", &s);
                }
                Err(e) => {
                    log::warn!("workspace {id}: {e:#}");
                    crate::tray::notify("Workspace not restored", &format!("{e:#}"));
                }
            }
        })
        .ok();
}

/// Snapshot the desktop as "Workspace N" (or `name`) and open the editor.
pub fn capture_workspace(name: String) {
    std::thread::Builder::new()
        .name("workspace-capture".into())
        .spawn(move || {
            let Some(cfg) = workspaces_ready() else { return };
            match workspaces::capture_and_save(&data_dir(), &name, &cfg) {
                Ok(ws) => {
                    log::info!("workspace captured: {} ({} windows)", ws.name, ws.apps.len());
                    crate::tray::notify("Desktop captured", &format!("Saved \"{}\" with {}.", ws.name, plural(ws.apps.len(), "window")));
                    open_workspaces_editor();
                }
                Err(e) => {
                    log::warn!("workspace capture: {e:#}");
                    crate::tray::notify("Capture failed", &format!("{e:#}"));
                }
            }
        })
        .ok();
}

pub fn open_workspaces_editor() {
    crate::children::open(&crate::children::WORKSPACES, &["workspace", "edit"]);
}

/// Whether a key press matches the Workspaces hotkey.
pub(crate) fn hotkey_matches(hk: &workspaces::Hotkey, vk: u32, win: bool, ctrl: bool, alt: bool, shift: bool) -> bool {
    hk.is_set() && hk.code == vk && hk.win == win && hk.ctrl == ctrl && hk.alt == alt && hk.shift == shift
}

/// The key of a key-down event as (VK, win, ctrl, alt, shift); `None` for
/// auto-repeats and keys without a VK.
#[cfg(target_os = "macos")]
fn key_of(ev: &pm_hook::Event<'_>) -> Option<(u32, bool, bool, bool, bool)> {
    const KEYCODE: u32 = 9; // kCGKeyboardEventKeycode
    const AUTOREPEAT: u32 = 8; // kCGKeyboardEventAutorepeat
    if ev.integer_field(AUTOREPEAT) != 0 {
        return None;
    }
    let (vk, _) = pm_input::vk::mac_to_vk(ev.integer_field(KEYCODE) as u16)?;
    let f = ev.flags();
    Some((vk, f & 0x0010_0000 != 0, f & 0x0004_0000 != 0, f & 0x0008_0000 != 0, f & 0x0002_0000 != 0))
}

#[cfg(windows)]
fn key_of(ev: &pm_hook::Event<'_>) -> Option<(u32, bool, bool, bool, bool)> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};
    let (_, info) = ev.keyboard()?;
    let down = |k: u16| unsafe { GetAsyncKeyState(k as i32) as u16 & 0x8000 != 0 };
    Some((info.vkCode, down(VK_LWIN) || down(VK_RWIN), down(VK_CONTROL), down(VK_MENU), down(VK_SHIFT)))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn key_of(_ev: &pm_hook::Event<'_>) -> Option<(u32, bool, bool, bool, bool)> {
    None
}

/// Listen for the Workspaces hotkey (opens the editor). Dropping the guard
/// stops listening.
pub(crate) fn workspaces_hotkey(hk: workspaces::Hotkey) -> pm_hook::Guard {
    use pm_hook::{EventMask, EventType, Subscription, Verdict};
    // Between Clipboard History (450) and Display Control (400): a local
    // shortcut like the other hotkeys; remapped keys arrive first.
    const PRIORITY: i32 = 440;
    log::info!("workspaces: editor shortcut {hk}");
    pm_hook::subscribe(
        Subscription::new("workspaces", PRIORITY, EventMask::of(&[EventType::KeyDown]), move |ev| {
            if ev.kind() != EventType::KeyDown {
                return Verdict::Pass;
            }
            match key_of(ev) {
                Some((vk, w, c, a, s)) if hotkey_matches(&hk, vk, w, c, a, s) => {
                    pm_hook::defer(open_workspaces_editor);
                    Verdict::Swallow
                }
                _ => Verdict::Pass,
            }
        })
        .ignore_tags(&[pm_hook::tags::MWB]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_arguments() {
        let a = restart_args(Path::new("/tmp/x/config.json"), 42, Some("permissions"), false);
        assert_eq!(a, ["--config", "/tmp/x/config.json", "run", "--wait-pid", "42", "--open-settings", "permissions"]);
        assert_eq!(restart_args(Path::new("c.json"), 1, None, false).len(), 5);
        assert_eq!(restart_args(Path::new("c.json"), 1, None, true).last().map(String::as_str), Some("--no-tray"));
        assert_eq!(bundle_of(Path::new("/Applications/PowerMac.app/Contents/MacOS/powermac")), Some(PathBuf::from("/Applications/PowerMac.app")));
        assert_eq!(bundle_of(Path::new("/Users/me/PowerMac/target/release/powermac")), None);
    }

    #[test]
    fn hotkey_matching() {
        let hk = workspaces::Hotkey::new(true, true, false, false, 0xC0);
        assert!(hotkey_matches(&hk, 0xC0, true, true, false, false));
        assert!(!hotkey_matches(&hk, 0xC0, true, true, false, true), "extra Shift");
        assert!(!hotkey_matches(&hk, 0x41, true, true, false, false));
        assert!(!hotkey_matches(&workspaces::Hotkey::default(), 0, false, false, false, false), "unset never matches");
    }

    #[test]
    fn module_status_lines() {
        let row = |enabled, running, needs: &'static [Perm]| ModuleRow { page: "x", name: "X", enabled, running, needs, detail: "2 displays".into() };
        let none = Probe::default();
        let all = Probe::all_granted();
        let rows = [row(false, false, &[]), row(true, true, &[]), row(true, false, &[]), row(true, true, &[Perm::Accessibility])];
        let st: Vec<ModuleState> = module_statuses(&rows, &all).iter().map(|s| s.state).collect();
        assert_eq!(st, [ModuleState::Off, ModuleState::Running, ModuleState::Failed, ModuleState::Running]);
        let s = module_statuses(&rows, &none);
        if cfg!(target_os = "macos") {
            assert_eq!(s[3].state, ModuleState::WaitingForPermission);
            assert_eq!(s[3].detail, "needs Accessibility");
        }
        assert_eq!(s[1].detail, "2 displays");
        assert_eq!(s[0].detail, "");
    }

    #[test]
    fn startup_page_follows_the_setup() {
        let mut c = AppConfig::default();
        assert_eq!(startup_page(&c), Some("welcome"));
        c.setup.completed = true;
        c.setup.remind_missing_permissions = false;
        assert_eq!(startup_page(&c), None);
    }
}
