//! Platforms without a backend: everything reports "unsupported".

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::backend::{Backend, LaunchRequest, LiveWindow, Placement};
use crate::capture::CaptureOptions;
use crate::geometry::{Monitor, Rect};
use crate::matching::InstalledApp;

pub fn has_permissions() -> bool {
    false
}

pub fn monitors() -> Result<Vec<Monitor>> {
    bail!("Workspaces is not supported on this platform")
}

pub fn installed_apps() -> Vec<InstalledApp> {
    vec![]
}

pub fn app_icon_rgba(_path: &str, _bundle_id: &str, _size: u32) -> Option<Vec<u8>> {
    None
}

pub fn app_exists(path: &str, _bundle_id: &str) -> bool {
    Path::new(path).exists()
}

pub fn pick_file(_prompt: &str) -> Option<PathBuf> {
    None
}

pub fn reveal(_path: &Path) {}

pub fn become_foreground_app() {}

pub struct Native {
    start: Instant,
}

impl Native {
    pub fn new(_opts: &CaptureOptions) -> Self {
        Self { start: Instant::now() }
    }
}

impl Backend for Native {
    fn monitors(&self) -> Result<Vec<Monitor>> {
        monitors()
    }
    fn windows(&self) -> Result<Vec<LiveWindow>> {
        bail!("Workspaces is not supported on this platform")
    }
    fn launch(&self, _req: &LaunchRequest) -> Result<()> {
        bail!("Workspaces is not supported on this platform")
    }
    fn place(&self, _w: &LiveWindow, _p: &Placement) -> Result<Option<String>> {
        bail!("Workspaces is not supported on this platform")
    }
    fn window_frame(&self, _w: &LiveWindow) -> Option<Rect> {
        None
    }
    fn minimize(&self, _w: &LiveWindow) -> Result<()> {
        bail!("unsupported")
    }
    fn hide_app(&self, _pid: i64) -> Result<()> {
        bail!("unsupported")
    }
    fn activate(&self, _w: &LiveWindow) {}
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
}
