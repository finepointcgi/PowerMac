//! Unsupported platforms.

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Result};

use super::CaptureCallback;
use crate::text::Fragment;
use crate::{Config, Hotkey};

pub struct HotkeyReg;

impl HotkeyReg {
    pub fn register(_hk: Hotkey, _cb: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        Ok(Self)
    }
}

pub fn screen_recording_granted() -> bool {
    true
}
pub fn request_screen_recording() -> bool {
    true
}
pub fn ocr_file(_path: &Path, _cfg: &Config) -> Result<Vec<Fragment>> {
    bail!("OCR is not supported on this platform")
}
pub fn ocr_rgba(_w: usize, _h: usize, _rgba: &[u8], _cfg: &Config) -> Result<Vec<Fragment>> {
    bail!("OCR is not supported on this platform")
}
pub fn supported_languages() -> Vec<String> {
    Vec::new()
}
pub fn capture_selection(_cfg: Config, cb: CaptureCallback) {
    cb(Err(anyhow::anyhow!(
        "Text Extractor is not supported on this platform"
    )));
}
pub fn toast(_text: &str) {}
pub fn capture_blocking(_cfg: Config) -> Result<Option<String>> {
    bail!("Text Extractor is not supported on this platform")
}
pub fn cancel_selection() {}
