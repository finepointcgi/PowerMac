//! Display Scaling glue for the settings window (a separate process that
//! never creates virtual displays or switches modes itself): this app writes
//! `display-scaling-status.json` (displays, their modes and the pending
//! "keep this resolution?" changes) every 2 s, every 0.5 s while a change
//! is pending, and applies the apply / confirm / revert requests the
//! settings window writes into `display-scaling-request.json`. A confirmed
//! mode is saved as the display's `preferredMode`, so it comes back on the
//! next connect / start.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use pm_config::bridge::{self, RequestReader, ScalingRequest, ScalingStatus, SCALING_REQUEST_FILE, SCALING_STATUS_FILE};
use pm_config::AppConfig;

use crate::RUNNING;

/// The last request's error (shown by the settings window).
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

/// Start the status / request thread (once per process). It idles while
/// Display Scaling is off.
pub fn start_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(path) = config_path() else { return };
        let _ = std::thread::Builder::new().name("scaling-status".into()).spawn(move || run(&path));
    });
}

fn run(path: &Path) {
    let mut reader = RequestReader::new(path, SCALING_REQUEST_FILE);
    let mut written = Instant::now() - Duration::from_secs(10);
    let mut pending = false;
    loop {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(req) = reader.poll::<ScalingRequest>(path, SCALING_REQUEST_FILE) {
            let r = apply_request(path, &req);
            *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = r.err().map(|e| {
                log::warn!("display scaling: {e:#}");
                format!("{e:#}")
            });
            written = Instant::now() - Duration::from_secs(10);
        }
        let every = if pending { Duration::from_millis(500) } else { Duration::from_secs(2) };
        if written.elapsed() >= every {
            written = Instant::now();
            if let Some(p) = write_status(path) {
                pending = p;
            }
        }
    }
}

fn apply_request(path: &Path, req: &ScalingRequest) -> anyhow::Result<()> {
    let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let Some(h) = guard.as_ref().and_then(|r| r.scaling.as_ref()) else {
        anyhow::bail!("Display Scaling isn't running");
    };
    match req {
        ScalingRequest::Apply { display_id, mode, permanent } => {
            let timeout = current_config(path).display_scaling.revert_timeout();
            h.apply_mode_with_timeout(display_id, mode, timeout, *permanent)?;
            log::info!("display scaling: {display_id} → {} (reverts in {} s unless kept)", mode.label(), timeout.as_secs());
        }
        ScalingRequest::Confirm { token } => {
            let change = h.pending().into_iter().find(|p| p.token == *token);
            h.confirm(*token)?;
            if let Some(c) = change {
                let display = h.displays().into_iter().find(|d| d.id == c.display_id);
                drop(guard);
                log::info!("display scaling: kept {} on {}", c.applied.label(), c.display_id);
                remember_mode(&c.display_id, display.as_ref(), &c.applied);
            }
        }
        ScalingRequest::Revert { token } => {
            h.revert(*token)?;
            log::info!("display scaling: change reverted");
        }
    }
    Ok(())
}

/// Save `mode` as the display's preferred mode (in the entry that already
/// matches it, else under its id).
fn remember_mode(id: &str, display: Option<&display_scaling::DisplayModes>, mode: &display_scaling::Mode) {
    let r = crate::edit_config(|c| {
        let displays = &mut c.display_scaling.displays;
        let (vendor, model) = display.map(|d| (d.vendor, d.model)).unwrap_or((0, 0));
        let key = display_scaling::logic::match_config(id, vendor, model, displays)
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| id.to_string());
        displays.entry(key).or_default().preferred_mode = Some(mode.spec());
    });
    if let Err(e) = r {
        log::warn!("display scaling: saving the mode: {e:#}");
    }
}

fn current_config(path: &Path) -> AppConfig {
    AppConfig::load_or_default(path).unwrap_or_default()
}

/// Write the status; `Some(any pending)` when the module runs.
fn write_status(path: &Path) -> Option<bool> {
    let st = {
        let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        let h = guard.as_ref().and_then(|r| r.scaling.as_ref())?;
        ScalingStatus {
            running: true,
            hidpi_supported: hidpi_supported(),
            displays: h.displays(),
            pending: h.pending(),
            error: LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        }
    };
    let pending = !st.pending.is_empty();
    if let Err(e) = bridge::write(path, SCALING_STATUS_FILE, &st) {
        log::debug!("display scaling: status file: {e:#}");
    }
    Some(pending)
}

fn hidpi_supported() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(display_scaling::hidpi_supported)
}

/// Tell the settings window Display Scaling is off.
pub fn write_stopped() {
    if let Some(p) = config_path() {
        let _ = bridge::write(&p, SCALING_STATUS_FILE, &ScalingStatus::default());
    }
}
