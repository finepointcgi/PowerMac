//! Menu bar manager glue for the settings window: `menubar-status.json`
//! (permissions, and the menu bar items with their sections while the
//! settings page asks for them) and the List / Move / Show / Hide requests
//! from `menubar-request.json`.
//!
//! Listing and moving items wait for the main thread (the manager lays the
//! menu bar out there), so they run on this thread with a clone of the
//! handle and never while holding `RUNNING`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use menubar_manager::{MoveTarget, Section};
use pm_config::bridge::{self, MenuBarRequest, MenuBarStatus, RequestReader, MENUBAR_REQUEST_FILE, MENUBAR_STATUS_FILE};
use pm_config::AppConfig;

use crate::RUNNING;

static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

/// The running manager (a clone of the handle, without holding `RUNNING`
/// afterwards). Gives up while the modules are being reconfigured.
pub(crate) fn handle() -> Option<Arc<menubar_manager::Handle>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|r| r.menubar.clone())
}

pub fn start_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(path) = config_path() else { return };
        let _ = std::thread::Builder::new().name("menubar-status".into()).spawn(move || run(&path));
    });
}

fn run(path: &Path) {
    let mut reader = RequestReader::new(path, MENUBAR_REQUEST_FILE);
    let mut written = Instant::now() - Duration::from_secs(60);
    // Items are listed only while the settings page keeps asking (listing
    // asks every app over Accessibility on macOS 26+).
    let mut list_until = Instant::now() - Duration::from_secs(1);
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if let Some(req) = reader.poll::<MenuBarRequest>(path, MENUBAR_REQUEST_FILE) {
            if req != MenuBarRequest::List {
                apply_request(req);
            }
            list_until = Instant::now() + Duration::from_secs(8);
            written = Instant::now() - Duration::from_secs(60);
        }
        let listing = list_until > Instant::now();
        let every = if listing { Duration::from_secs(2) } else { Duration::from_secs(5) };
        if written.elapsed() >= every {
            written = Instant::now();
            write_status(path, listing);
        }
    }
}

fn apply_request(req: MenuBarRequest) {
    let Some(h) = handle() else { return };
    let r = match req {
        MenuBarRequest::List => Ok(()),
        MenuBarRequest::ShowHidden => {
            h.show_hidden();
            Ok(())
        }
        MenuBarRequest::Hide => {
            h.hide();
            Ok(())
        }
        MenuBarRequest::Move { item_id, section } => {
            // A drag across the top center must not open the notch shelf.
            if let Ok(g) = RUNNING.try_lock() {
                if let Some(s) = g.as_ref().and_then(|r| r.shelf.as_ref()) {
                    s.collapse();
                }
            }
            let r = h.move_item(&item_id, MoveTarget::Section(section));
            if r.is_ok() {
                log::info!("menu bar: moved {item_id} to {}", section_name(section));
            }
            r
        }
    };
    *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = r.err().map(|e| {
        log::warn!("menu bar: {e:#}");
        format!("{e:#}")
    });
}

pub(crate) fn section_name(s: Section) -> &'static str {
    match s {
        Section::Visible => "Visible",
        Section::Hidden => "Hidden",
        Section::AlwaysHidden => "Always hidden",
    }
}

fn write_status(path: &Path, listing: bool) {
    let Some(h) = handle() else { return };
    let p = menubar_manager::permissions();
    let mut st = MenuBarStatus {
        running: true,
        accessibility: p.accessibility,
        screen_recording: p.screen_recording,
        fitted_mode: h.fitted_mode(),
        layout_issue: h.layout_issue(),
        error: LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        ..Default::default()
    };
    if listing {
        st.items = h.items();
        st.hidden_by_notch = h.items_hidden_by_notch();
        st.listed = true;
    }
    if let Err(e) = bridge::write(path, MENUBAR_STATUS_FILE, &st) {
        log::debug!("menu bar: status file: {e:#}");
    }
}

pub fn write_stopped() {
    if let Some(p) = config_path() {
        let _ = bridge::write(&p, MENUBAR_STATUS_FILE, &MenuBarStatus::default());
    }
}
