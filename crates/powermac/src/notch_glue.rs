//! Notch Shelf glue: the settings window (another process) sees the
//! widgets' latest results, the reminder lists and the permission states
//! through `notch-status.json` (written every 2 s while the shelf runs) and
//! asks for a widget refresh or a Home page through `notch-request.json`
//! (polled every 250 ms).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use notch_shelf::{Access, WidgetKind};
use pm_config::bridge::{self, NotchRequest, NotchStatus, RequestReader, WidgetPreview, NOTCH_REQUEST_FILE, NOTCH_STATUS_FILE};
use pm_config::AppConfig;

use crate::RUNNING;

/// Ids of the instances whose results go into the status file (Script,
/// xbar, Web), set whenever the shelf's config is applied.
static PREVIEW_IDS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

/// Remember which instances to report (call with each applied config).
pub fn set_config(cfg: &notch_shelf::Config) {
    let ids = cfg
        .widgets
        .iter()
        .filter(|w| matches!(w.kind, WidgetKind::Script | WidgetKind::Xbar | WidgetKind::Web))
        .map(|w| w.id.clone())
        .collect();
    *PREVIEW_IDS.lock().unwrap_or_else(|p| p.into_inner()) = ids;
}

/// Start the status / request thread (once per process). It idles while
/// the shelf is off.
pub fn start_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(path) = config_path() else { return };
        let _ = std::thread::Builder::new().name("notch-status".into()).spawn(move || run(&path));
    });
}

fn run(path: &Path) {
    let mut reader = RequestReader::new(path, NOTCH_REQUEST_FILE);
    let mut written = Instant::now() - Duration::from_secs(10);
    loop {
        std::thread::sleep(Duration::from_millis(250));
        if let Some(req) = reader.poll::<NotchRequest>(path, NOTCH_REQUEST_FILE) {
            apply_request(req);
            // Report the refreshed result soon.
            written = Instant::now() - Duration::from_millis(1_000);
        }
        if written.elapsed() >= Duration::from_secs(2) {
            written = Instant::now();
            write_status(path);
        }
    }
}

fn apply_request(req: NotchRequest) {
    let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let Some(h) = guard.as_ref().and_then(|r| r.shelf.as_ref()) else { return };
    match req {
        NotchRequest::RefreshWidget { id } => h.refresh_widget(&id),
        NotchRequest::ShowPage { page } => {
            h.set_view(notch_shelf::View::Home);
            h.set_page(page);
            h.expand();
        }
    }
}

fn access_name(a: Access) -> &'static str {
    match a {
        Access::Unknown => "unknown",
        Access::NotDetermined => "notDetermined",
        Access::Requesting => "requesting",
        Access::Granted => "granted",
        Access::Denied => "denied",
        Access::MissingUsageKey => "missingUsageKey",
        Access::Unavailable => "unavailable",
    }
}

/// The status of a running shelf (`None` while it's off).
fn snapshot() -> Option<NotchStatus> {
    let ids = PREVIEW_IDS.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let h = guard.as_ref().and_then(|r| r.shelf.as_ref())?;
    let st = h.widget_status();
    let mut out = NotchStatus {
        running: true,
        calendar: access_name(st.calendar).into(),
        camera: access_name(st.camera).into(),
        reminders: access_name(st.reminders).into(),
        reminder_lists: h.reminder_lists(),
        widgets: Default::default(),
    };
    for id in ids {
        if let Some(r) = h.widget_content(&id) {
            out.widgets.insert(id, WidgetPreview::from_result(&r));
        }
    }
    Some(out)
}

fn write_status(path: &Path) {
    let Some(st) = snapshot() else { return };
    if let Err(e) = bridge::write(path, NOTCH_STATUS_FILE, &st) {
        log::debug!("notch shelf: status file: {e:#}");
    }
}

/// Tell the settings window the shelf is off.
pub fn write_stopped() {
    if let Some(p) = config_path() {
        let _ = bridge::write(&p, NOTCH_STATUS_FILE, &NotchStatus::default());
    }
}
