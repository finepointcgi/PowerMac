//! OS backends. Each backend provides:
//!
//! * `NativeWindowSystem` — implements [`crate::engine::WindowSystem`].
//! * `Runtime` — owns hook threads / overlay; created by `FancyZones::start`.
//! * `has_permissions()` — whether the process may control other windows.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use self::windows::*;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use unsupported::*;
