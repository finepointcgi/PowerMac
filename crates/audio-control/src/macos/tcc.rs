//! The audio-capture permission (TCC service `kTCCServiceAudioCapture`) that
//! process taps need. There is no public API to query it, so — like Apple's
//! own sample-adjacent tools (e.g. AudioCap) — we call the private
//! `TCCAccessPreflight` / `TCCAccessRequest` from TCC.framework, resolved with
//! dlsym so nothing breaks if they go away.

use crate::CapturePermission;
use anyhow::{anyhow, Result};
use block2::RcBlock;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;
use objc2::runtime::Bool;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;
use std::time::Duration;

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NOW: c_int = 2;
const SERVICE: &str = "kTCCServiceAudioCapture";

type PreflightFn = unsafe extern "C" fn(CFStringRef, CFDictionaryRef) -> c_int;
type RequestFn = unsafe extern "C" fn(CFStringRef, CFDictionaryRef, *const block2::Block<dyn Fn(Bool)>);

pub(crate) fn sym(lib: &CStr, name: &CStr) -> *mut c_void {
    unsafe {
        let h = dlopen(lib.as_ptr(), RTLD_NOW);
        if h.is_null() {
            return std::ptr::null_mut();
        }
        dlsym(h, name.as_ptr())
    }
}

fn tcc() -> (Option<PreflightFn>, Option<RequestFn>) {
    static FNS: OnceLock<(usize, usize)> = OnceLock::new();
    let (p, r) = *FNS.get_or_init(|| {
        let lib = c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC";
        (sym(lib, c"TCCAccessPreflight") as usize, sym(lib, c"TCCAccessRequest") as usize)
    });
    unsafe {
        (
            (p != 0).then(|| std::mem::transmute::<usize, PreflightFn>(p)),
            (r != 0).then(|| std::mem::transmute::<usize, RequestFn>(r)),
        )
    }
}

/// Current state without prompting.
pub(crate) fn preflight() -> CapturePermission {
    let (Some(pre), _) = tcc() else { return CapturePermission::NotApplicable };
    let svc = CFString::new(SERVICE);
    match unsafe { pre(svc.as_concrete_TypeRef(), std::ptr::null()) } {
        0 => CapturePermission::Granted,
        1 => CapturePermission::Denied,
        _ => CapturePermission::NotDetermined,
    }
}

/// Show the system prompt (only shows when not determined yet) and wait for
/// the answer.
pub(crate) fn request() -> Result<bool> {
    let (_, Some(req)) = tcc() else { return Err(anyhow!("TCCAccessRequest is unavailable")) };
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    let tx = std::sync::Mutex::new(tx);
    let block = RcBlock::new(move |granted: Bool| {
        if let Ok(t) = tx.lock() {
            let _ = t.send(granted.as_bool());
        }
    });
    let svc = CFString::new(SERVICE);
    unsafe { req(svc.as_concrete_TypeRef(), std::ptr::null(), &*block) };
    rx.recv_timeout(Duration::from_secs(300)).map_err(|_| anyhow!("no answer to the audio capture prompt"))
}
