//! Windows (and anything else): no-op. The Windows notification area
//! already has an overflow flyout with drag-to-arrange, which is what this
//! module adds on macOS.

use std::sync::Arc;

use anyhow::{bail, Result};

use crate::{Permissions, RawItem, Shared, Snapshot};

pub struct Backend;

impl Backend {
    pub fn start(_shared: Arc<Shared>) -> Result<Backend> {
        Ok(Backend)
    }
    pub fn wake(&self) {}
    pub fn stop(&self) {}
}

pub fn permissions() -> Permissions {
    Permissions { accessibility: false, screen_recording: false }
}

pub fn snapshot_now() -> Snapshot {
    Snapshot::default()
}

pub fn reset_positions() {}

pub fn list_other_items() -> Vec<RawItem> {
    vec![]
}

pub fn is_main_thread() -> bool {
    false
}

pub fn refresh_on_main() {}

pub fn cmd_drag(_path: &[(f64, f64)], _pid: i32) -> Result<()> {
    bail!("not supported on this platform")
}

pub fn scan_visible(_screen: crate::Rect, _menu_bar_h: f64) -> Vec<(f64, i32, Option<String>)> {
    vec![]
}

pub fn fitted_mode_default() -> bool {
    false
}

pub fn really_shown(_frame: crate::Rect, _pid: i32) -> Option<bool> {
    None
}
