//! Awake: keep the computer awake (port of PowerToys Awake) for macOS and Windows.
//!
//! ```no_run
//! use std::time::Duration;
//! use awake::{Awake, AwakeConfig};
//!
//! let awake = Awake::new()?;
//! awake.set(AwakeConfig::timed(Duration::from_secs(3600), true))?;
//! println!("{:?}", awake.status().remaining);
//! awake.stop();
//! # Ok::<(), awake::AwakeError>(())
//! ```
//!
//! * macOS: IOKit `IOPMAssertionCreateWithName` (`PreventUserIdleSystemSleep`,
//!   plus `PreventUserIdleDisplaySleep` when `keep_display_on`).
//! * Windows: `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED
//!   [| ES_DISPLAY_REQUIRED])` on the controller's dedicated worker thread.

mod controller;
pub mod engine;
mod mode;
pub mod platform;

pub use controller::Awake;
pub use engine::{Clock, Engine, PowerBackend, ProcessProbe, SystemClock};
pub use mode::{
    parse_expire_at, AwakeConfig, AwakeError, AwakeMode, AwakeStatus, EndReason, PowerRequest,
    PowerToysAwakeProperties,
};

#[cfg(test)]
mod tests;
