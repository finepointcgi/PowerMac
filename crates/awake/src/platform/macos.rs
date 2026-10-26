//! macOS backend: IOKit power assertions + `kill(pid, 0)` process probing.

use std::ffi::{c_char, c_void, CString};

use crate::engine::{PowerBackend, ProcessProbe};
use crate::mode::{AwakeError, PowerRequest};

type CFStringRef = *const c_void;
type CFAllocatorRef = *const c_void;
type IOPMAssertionID = u32;
type IOPMAssertionLevel = u32;
type IOReturn = i32;

const K_IOPM_ASSERTION_LEVEL_ON: IOPMAssertionLevel = 255;
const K_IO_RETURN_SUCCESS: IOReturn = 0;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// `kIOPMAssertionTypePreventUserIdleSystemSleep`
pub const ASSERT_SYSTEM: &str = "PreventUserIdleSystemSleep";
/// `kIOPMAssertionTypePreventUserIdleDisplaySleep`
pub const ASSERT_DISPLAY: &str = "PreventUserIdleDisplaySleep";

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: CFStringRef,
        level: IOPMAssertionLevel,
        name: CFStringRef,
        id: *mut IOPMAssertionID,
    ) -> IOReturn;
    fn IOPMAssertionRelease(id: IOPMAssertionID) -> IOReturn;
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Owned CFString, released on drop.
struct CfString(CFStringRef);

impl CfString {
    fn new(s: &str) -> Result<Self, AwakeError> {
        let c = CString::new(s).map_err(|e| AwakeError::Backend(e.to_string()))?;
        // SAFETY: valid NUL-terminated UTF-8 string; NULL allocator = default.
        let r = unsafe {
            CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        };
        if r.is_null() {
            Err(AwakeError::Backend("CFStringCreateWithCString failed".into()))
        } else {
            Ok(Self(r))
        }
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        // SAFETY: we own this reference (Create rule).
        unsafe { CFRelease(self.0) }
    }
}

/// Holds zero or more IOPM assertions; `apply` releases the old ones first.
pub struct MacPowerBackend {
    held: Vec<IOPMAssertionID>,
    name: String,
}

impl MacPowerBackend {
    pub fn new() -> Self {
        Self::with_name("PowerMac Awake")
    }

    /// `name` is shown by `pmset -g assertions`.
    pub fn with_name(name: impl Into<String>) -> Self {
        Self { held: Vec::new(), name: name.into() }
    }

    fn create(&mut self, kind: &str) -> Result<(), AwakeError> {
        let ty = CfString::new(kind)?;
        let name = CfString::new(&self.name)?;
        let mut id: IOPMAssertionID = 0;
        // SAFETY: valid CFStrings and out-pointer.
        let rc = unsafe {
            IOPMAssertionCreateWithName(ty.0, K_IOPM_ASSERTION_LEVEL_ON, name.0, &mut id)
        };
        if rc != K_IO_RETURN_SUCCESS {
            return Err(AwakeError::Backend(format!(
                "IOPMAssertionCreateWithName({kind}) failed: 0x{rc:08x}"
            )));
        }
        log::debug!("awake: created IOPM assertion {kind} id={id}");
        self.held.push(id);
        Ok(())
    }

    fn release_all(&mut self) {
        for id in self.held.drain(..) {
            // SAFETY: id came from IOPMAssertionCreateWithName.
            let rc = unsafe { IOPMAssertionRelease(id) };
            if rc != K_IO_RETURN_SUCCESS {
                log::warn!("awake: IOPMAssertionRelease({id}) failed: 0x{rc:08x}");
            }
        }
    }

    /// IDs currently held (for diagnostics).
    pub fn assertion_ids(&self) -> &[u32] {
        &self.held
    }
}

impl Default for MacPowerBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerBackend for MacPowerBackend {
    fn apply(&mut self, req: PowerRequest) -> Result<(), AwakeError> {
        self.release_all();
        let r = match req {
            PowerRequest::Release => Ok(()),
            PowerRequest::System => self.create(ASSERT_SYSTEM),
            PowerRequest::SystemAndDisplay => {
                self.create(ASSERT_SYSTEM).and_then(|_| self.create(ASSERT_DISPLAY))
            }
        };
        if r.is_err() {
            self.release_all();
        }
        r
    }
}

impl Drop for MacPowerBackend {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// Probes liveness with `kill(pid, 0)` (EPERM still means "exists").
/// Note: a zombie that has not been reaped still counts as alive.
#[derive(Default)]
pub struct MacProcessProbe {
    pid: Option<i32>,
}

pub(crate) fn pid_exists(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 performs error checking only.
    let rc = unsafe { kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    const EPERM: i32 = 1;
    std::io::Error::last_os_error().raw_os_error() == Some(EPERM)
}

impl ProcessProbe for MacProcessProbe {
    fn bind(&mut self, pid: u32) -> bool {
        let Ok(p) = i32::try_from(pid) else { return false };
        if pid_exists(p) {
            self.pid = Some(p);
            true
        } else {
            false
        }
    }
    fn is_alive(&mut self) -> bool {
        self.pid.is_none_or(pid_exists)
    }
    fn unbind(&mut self) {
        self.pid = None;
    }
}
