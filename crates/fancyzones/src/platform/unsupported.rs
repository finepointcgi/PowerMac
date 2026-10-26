//! Stub backend for platforms without an implementation.

use std::sync::Arc;

use anyhow::{bail, Result};

use crate::engine::{Monitor, WindowKey, WindowSystem};
use crate::geometry::Rect;
use crate::Shared;

pub fn has_permissions() -> bool {
    false
}

#[derive(Default)]
pub struct NativeWindowSystem;

impl NativeWindowSystem {
    pub fn new() -> Self {
        Self
    }
}

impl WindowSystem for NativeWindowSystem {
    type Window = ();
    fn monitors(&self) -> Result<Vec<Monitor>> {
        bail!("fancyzones: unsupported platform")
    }
    fn focused_window(&self) -> Result<Option<()>> {
        bail!("fancyzones: unsupported platform")
    }
    fn window_key(&self, _: &()) -> WindowKey {
        0
    }
    fn window_rect(&self, _: &()) -> Result<Rect> {
        bail!("fancyzones: unsupported platform")
    }
    fn set_window_rect(&self, _: &(), _: Rect) -> Result<()> {
        bail!("fancyzones: unsupported platform")
    }
}

pub fn monitor_under_cursor() -> Option<String> {
    None
}

pub(crate) struct Runtime;

impl Runtime {
    pub(crate) fn start(_: Arc<Shared>) -> Result<Self> {
        bail!("fancyzones: unsupported platform")
    }
    pub(crate) fn stop(&mut self) {}
}
