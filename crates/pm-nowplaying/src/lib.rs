//! Now Playing helper for the Notch Shelf.
//!
//! Since macOS 15.4 MediaRemote only answers Apple processes, so this
//! library is loaded into `/usr/bin/perl` (Apple-signed, no library
//! validation) and does the MediaRemote calls from there: the current item
//! with its artwork, and transport commands. The host runs:
//!
//! ```text
//! /usr/bin/perl -e '<loader>' /path/to/libpm_nowplaying.dylib
//! ```
//!
//! with `PM_NP_MODE` set, and the loader calls [`pm_np_main`] as an XSUB.
//!
//! `PM_NP_MODE=get` prints one line:
//! `np \t rate \t title \t artist \t album \t duration \t elapsed \t
//! sampled-at (unix) \t player \t artwork hash \t artwork (base64)`, or
//! `none`. The artwork is left empty when its hash equals `PM_NP_ART`.
//!
//! `PM_NP_MODE=send` with `PM_NP_CMD=<MRCommand>` prints `sent 1` / `sent 0`.

#[cfg(target_os = "macos")]
mod mac;

/// XSUB entry point: perl passes its interpreter and the CV, both unused
/// (no arguments, no return values).
#[no_mangle]
pub extern "C" fn pm_np_main(_interp: *mut std::ffi::c_void, _cv: *mut std::ffi::c_void) {
    #[cfg(target_os = "macos")]
    {
        let out = std::panic::catch_unwind(mac::run).unwrap_or_else(|_| "error\n".to_string());
        use std::io::Write;
        let mut so = std::io::stdout().lock();
        let _ = so.write_all(out.as_bytes());
        let _ = so.flush();
    }
}
