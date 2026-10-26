//! Platform backends.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{hidpi_supported, new_backend};
#[cfg(target_os = "macos")]
pub use macos::virtual_display_self_test;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub(crate) use windows::{hidpi_supported, new_backend};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) use unsupported::{hidpi_supported, new_backend};
