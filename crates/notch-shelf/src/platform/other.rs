//! No-op backend for platforms without a shelf UI.

use std::sync::Arc;

use anyhow::Result;

use crate::Shared;

pub fn has_notch() -> bool {
    false
}

pub struct Backend;

impl Backend {
    pub fn start(_shared: Arc<Shared>) -> Result<Backend> {
        log::info!("notch-shelf: no UI on this platform");
        Ok(Backend)
    }
    pub fn wake(&self) {}
    pub fn stop(&self) {}
}
