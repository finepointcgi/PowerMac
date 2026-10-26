//! Native backends. `NativePowerBackend` / `NativeProcessProbe` alias the
//! implementation for the current OS (a logging no-op elsewhere).

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "macos")]
pub use macos::{MacPowerBackend as NativePowerBackend, MacProcessProbe as NativeProcessProbe};
#[cfg(windows)]
pub use self::windows::{WinPowerBackend as NativePowerBackend, WinProcessProbe as NativeProcessProbe};

#[cfg(not(any(target_os = "macos", windows)))]
mod unsupported {
    use crate::engine::{PowerBackend, ProcessProbe};
    use crate::mode::{AwakeError, PowerRequest};

    /// No-op backend for unsupported platforms.
    #[derive(Default)]
    pub struct NativePowerBackend;
    impl NativePowerBackend {
        pub fn new() -> Self {
            Self
        }
    }
    impl PowerBackend for NativePowerBackend {
        fn apply(&mut self, req: PowerRequest) -> Result<(), AwakeError> {
            log::warn!("awake: power requests unsupported on this platform ({req:?})");
            Ok(())
        }
    }
    /// Probe that assumes the process is alive.
    #[derive(Default)]
    pub struct NativeProcessProbe;
    impl ProcessProbe for NativeProcessProbe {
        fn bind(&mut self, _pid: u32) -> bool {
            true
        }
        fn is_alive(&mut self) -> bool {
            true
        }
        fn unbind(&mut self) {}
    }
}
#[cfg(not(any(target_os = "macos", windows)))]
pub use unsupported::{NativePowerBackend, NativeProcessProbe};
