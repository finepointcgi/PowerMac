//! No display control on this platform.

use anyhow::{bail, Result};

use crate::engine::{Backend, RawDisplay};
use crate::logic::{Target, VirtualPlan};
use crate::Mode;

struct Unsupported;

pub(crate) fn hidpi_supported() -> bool {
    false
}

pub(crate) fn new_backend() -> Box<dyn Backend> {
    Box::new(Unsupported)
}

impl Backend for Unsupported {
    fn enumerate(&mut self) -> Vec<RawDisplay> {
        Vec::new()
    }
    fn signature(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn set_mode(&mut self, _: &str, _: Target, _: &Mode, _: bool) -> Result<()> {
        bail!("not supported on this platform")
    }
    fn hidpi_supported(&self) -> bool {
        false
    }
    fn enable_hidpi(&mut self, _: &str, _: &VirtualPlan) -> Result<()> {
        bail!("not supported on this platform")
    }
    fn disable_hidpi(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
    fn virtual_modes(&mut self, _: &str) -> Option<(Option<Mode>, Vec<Mode>)> {
        None
    }
    fn repair_mirroring(&mut self, _: &[String]) {}
}
