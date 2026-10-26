//! Platform backends. Each provides:
//! * `new_backend() -> Box<dyn Backend>` (hardware access, worker thread),
//! * `KeyHook` (media keys + hotkeys),
//! * `close_osd` (the OSD itself is shown by the key hook).

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::*;
#[cfg(target_os = "macos")]
pub use macos::{debug_noop_writes, debug_report, debug_show_osd, run_main_loop_for};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub(crate) use windows::*;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) use unsupported::*;
