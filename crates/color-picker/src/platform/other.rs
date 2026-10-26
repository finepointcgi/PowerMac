//! Unsupported platforms: nothing to pick with, hotkeys are ignored.

use std::sync::Arc;

use crate::{Hotkey, Rgb};

pub type PickCallback = Box<dyn FnOnce(Option<Rgb>) + Send>;

pub struct HotkeyReg;

impl HotkeyReg {
    pub fn register(_hk: Hotkey, _cb: Arc<dyn Fn() + Send + Sync>) -> anyhow::Result<Self> {
        Ok(Self)
    }
}

pub fn pick_async(cb: PickCallback) {
    cb(None);
}

pub fn pick_blocking() -> Option<Rgb> {
    None
}
