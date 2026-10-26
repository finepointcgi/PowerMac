//! Stub for platforms without a backend.

use anyhow::{bail, Result};

use crate::config::Config;
use crate::model::{Mode, WindowInfo};

pub(crate) struct Runtime;

impl Runtime {
    pub(crate) fn start(_cfg: Config) -> Result<Self> {
        bail!("the window switcher is not supported on this platform")
    }
    pub(crate) fn update(&mut self, _cfg: Config) {}
    pub(crate) fn config(&self) -> Config {
        Config::default()
    }
    pub(crate) fn show(&self, _mode: Mode) {}
    pub(crate) fn hide(&self) {}
    pub(crate) fn is_shown(&self) -> bool {
        false
    }
    pub(crate) fn is_input_active(&self) -> bool {
        false
    }
    pub(crate) fn windows(&self) -> Vec<WindowInfo> {
        vec![]
    }
    pub(crate) fn stop(&mut self) {}
}

pub(crate) fn permissions() -> crate::Permissions {
    crate::Permissions { accessibility: false, screen_recording: false }
}
