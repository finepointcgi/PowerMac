//! Windows backend: `SetThreadExecutionState` + process handle waiting.
//!
//! `SetThreadExecutionState` is per-thread, so [`WinPowerBackend`] is `!Send`
//! and must be created and used on one long-lived thread (the controller's
//! worker thread does exactly that, mirroring PowerToys' monitor thread).

use std::marker::PhantomData;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    EXECUTION_STATE,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
};

use crate::engine::{PowerBackend, ProcessProbe};
use crate::mode::{AwakeError, PowerRequest};

pub struct WinPowerBackend {
    held: bool,
    _not_send: PhantomData<*const ()>,
}

impl WinPowerBackend {
    pub fn new() -> Self {
        Self { held: false, _not_send: PhantomData }
    }
}

impl Default for WinPowerBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn set_state(flags: EXECUTION_STATE) -> Result<(), AwakeError> {
    // SAFETY: plain Win32 call.
    let prev = unsafe { SetThreadExecutionState(flags) };
    if prev == 0 {
        Err(AwakeError::Backend(format!("SetThreadExecutionState(0x{flags:08x}) failed")))
    } else {
        Ok(())
    }
}

impl PowerBackend for WinPowerBackend {
    fn apply(&mut self, req: PowerRequest) -> Result<(), AwakeError> {
        let flags = match req {
            PowerRequest::Release => ES_CONTINUOUS,
            PowerRequest::System => ES_CONTINUOUS | ES_SYSTEM_REQUIRED,
            PowerRequest::SystemAndDisplay => {
                ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
            }
        };
        let r = set_state(flags);
        self.held = r.is_ok() && req != PowerRequest::Release;
        r
    }
}

impl Drop for WinPowerBackend {
    fn drop(&mut self) {
        if self.held {
            let _ = set_state(ES_CONTINUOUS);
        }
    }
}

/// Holds a SYNCHRONIZE handle to the bound process (immune to PID reuse) and
/// checks it with `WaitForSingleObject(h, 0)`.
#[derive(Default)]
pub struct WinProcessProbe {
    handle: Option<HANDLE>,
}

impl ProcessProbe for WinProcessProbe {
    fn bind(&mut self, pid: u32) -> bool {
        self.unbind();
        // SAFETY: plain Win32 call.
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if h.is_null() {
            return false;
        }
        self.handle = Some(h);
        self.is_alive()
    }
    fn is_alive(&mut self) -> bool {
        match self.handle {
            // SAFETY: handle is owned and open.
            Some(h) => (unsafe { WaitForSingleObject(h, 0) }) == WAIT_TIMEOUT,
            None => true,
        }
    }
    fn unbind(&mut self) {
        if let Some(h) = self.handle.take() {
            // SAFETY: we own this handle.
            unsafe { CloseHandle(h) };
        }
    }
}

impl Drop for WinProcessProbe {
    fn drop(&mut self) {
        self.unbind();
    }
}
