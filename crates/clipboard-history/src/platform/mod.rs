//! OS glue: clipboard read/write, frontmost app, activation, pointer
//! position, hotkey.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(any(target_os = "macos", windows)))]
mod unsupported {
    use crate::content::{Content, SourceApp};
    use anyhow::{bail, Result};
    use std::sync::Arc;
    use std::time::Duration;

    pub const POLL: Duration = Duration::from_millis(500);
    pub fn change_count() -> i64 {
        0
    }
    pub fn read_meta() -> (Vec<String>, Option<SourceApp>) {
        (Vec::new(), None)
    }
    pub fn read_content(_: &[String]) -> Content {
        Content::default()
    }
    pub fn write(_: &Content, _: bool) -> Result<i64> {
        bail!("unsupported platform")
    }
    #[derive(Debug, Clone)]
    pub struct FrontApp {
        pub pid: i32,
        pub window: isize,
        pub app: SourceApp,
    }
    pub fn frontmost() -> Option<FrontApp> {
        None
    }
    pub fn activate(_: i32, _: isize) -> bool {
        false
    }
    pub fn is_frontmost(_: i32) -> bool {
        false
    }
    pub fn pointer_and_screen() -> Option<([f32; 2], [f32; 4])> {
        None
    }
    pub struct Hotkey;
    impl Hotkey {
        pub fn start(
            _: keyboard_manager::Shortcut,
            _: Arc<dyn Fn() + Send + Sync>,
        ) -> Result<Hotkey> {
            bail!("unsupported platform")
        }
    }
}
#[cfg(not(any(target_os = "macos", windows)))]
pub use unsupported::*;
