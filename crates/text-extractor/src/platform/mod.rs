//! Platform backends: hotkeys, the selection overlay, screen capture, OCR
//! and the confirmation toast.

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

/// Called with the recognized text (`None` = cancelled).
pub type CaptureCallback = Box<dyn FnOnce(anyhow::Result<Option<String>>) + Send>;
