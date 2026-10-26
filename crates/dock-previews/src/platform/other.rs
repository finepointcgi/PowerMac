//! No-op backend: Windows already shows window thumbnails on the taskbar,
//! and there is no Dock elsewhere. Starting succeeds and does nothing.

use anyhow::Result;

use crate::Config;

pub(crate) struct Runtime {
    cfg: Config,
}

impl Runtime {
    pub(crate) fn start(cfg: Config) -> Result<Self> {
        log::info!("dock-previews: not needed on this platform (taskbar previews are built in)");
        Ok(Runtime { cfg })
    }
    pub(crate) fn update(&mut self, cfg: Config) {
        self.cfg = cfg;
    }
    pub(crate) fn config(&self) -> Config {
        self.cfg.clone()
    }
    pub(crate) fn is_shown(&self) -> bool {
        false
    }
    pub(crate) fn is_input_active(&self) -> bool {
        false
    }
    pub(crate) fn stop(&mut self) {}
}

pub(crate) fn permissions() -> crate::Permissions {
    crate::Permissions { accessibility: false, screen_recording: false }
}
