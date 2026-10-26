#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{permissions, Runtime};

#[cfg(not(target_os = "macos"))]
mod other;
#[cfg(not(target_os = "macos"))]
pub(crate) use other::{permissions, Runtime};
