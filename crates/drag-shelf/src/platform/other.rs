//! No shelves on this platform (Windows isn't supported yet): the model
//! works, nothing is shown.

use std::sync::Arc;

use anyhow::Result;

use crate::{Point, Shared};

pub fn cursor() -> Option<Point> {
    None
}

pub struct Backend;

impl Backend {
    pub fn start(_shared: Arc<Shared>) -> Result<Backend> {
        anyhow::bail!("{}", crate::unsupported_reason().unwrap_or("not supported"))
    }
    pub fn wake(&self) {}
    pub fn stop(&self) {}
    pub fn button_up() {}
    pub fn shake(_at: Point) {}
    pub fn hotkey() {}
}
