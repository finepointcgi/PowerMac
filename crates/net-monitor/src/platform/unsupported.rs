use anyhow::{bail, Result};

use crate::model::Snapshot;

pub fn snapshot() -> Result<Snapshot> {
    bail!("the network monitor isn't implemented on this platform")
}
