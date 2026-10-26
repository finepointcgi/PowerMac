//! Fallback for platforms without a backend: the engine runs (programmatic
//! triggers update state) but nothing is drawn and no input is captured.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::{InputMode, Shared};

pub fn double_click_time() -> Duration {
    Duration::from_millis(500)
}

pub(crate) struct Runtime;

impl Runtime {
    pub(crate) fn start(shared: Arc<Shared>) -> Result<Self> {
        shared.set_input_mode(InputMode::None);
        log::warn!("mouse-utils: no overlay/input backend for this OS");
        Ok(Runtime)
    }

    pub(crate) fn stop(&mut self) {}
}

pub fn run_main_loop_for(dur: Duration) {
    std::thread::sleep(dur);
}

pub fn debug_probe(offsets: &[(f64, f64)]) -> Vec<Option<[u8; 4]>> {
    vec![None; offsets.len()]
}
