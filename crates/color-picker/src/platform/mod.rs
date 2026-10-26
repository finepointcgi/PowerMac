//! Platform backends: picking a color from the screen and global hotkeys.

#[cfg(target_os = "macos")]
mod mac_hotkey;
#[cfg(target_os = "macos")]
pub(crate) mod mac_main;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(not(any(target_os = "macos", windows)))]
mod other;
#[cfg(not(any(target_os = "macos", windows)))]
pub use other::*;
