//! Platforms without a backend: no displays, no keys.

use std::sync::Arc;

use anyhow::{bail, Result};

use crate::engine::{Backend, Inner, RawDisplay, ReadError};

struct NoBackend;

impl Backend for NoBackend {
    fn enumerate(&mut self) -> Vec<RawDisplay> {
        Vec::new()
    }
    fn signature(&mut self) -> u64 {
        0
    }
    fn ddc_read(&mut self, _id: &str, _vcp: u8) -> Result<(u16, u16), ReadError> {
        Err(ReadError::Unsupported)
    }
    fn ddc_write(&mut self, _id: &str, _vcp: u8, _value: u16) -> Result<()> {
        bail!("unsupported platform")
    }
    fn native_get(&mut self, _id: &str) -> Option<f32> {
        None
    }
    fn native_set(&mut self, _id: &str, _level: f32) -> Result<()> {
        bail!("unsupported platform")
    }
    fn gamma_set(&mut self, _id: &str, _factor: f32) -> Result<()> {
        bail!("unsupported platform")
    }
    fn gamma_restore(&mut self) {}
}

pub(crate) fn new_backend() -> Box<dyn Backend> {
    Box::new(NoBackend)
}

pub(crate) struct KeyHook;

impl KeyHook {
    pub(crate) fn start(_inner: Arc<Inner>) -> Result<Self> {
        bail!("key hooks are not supported on this platform")
    }
    pub(crate) fn stop(&mut self) {}
}

pub(crate) fn close_osd() {}
