use std::ffi::{c_char, c_void};
use std::sync::mpsc;
use std::time::Duration;

use block2::{Block, RcBlock};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::{NSData, NSString};

extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dispatch_get_global_queue(id: isize, flags: usize) -> *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: u8) -> i32;
    static kCFRunLoopDefaultMode: *const c_void;
}

type GetInfoFn = unsafe extern "C" fn(queue: *const c_void, block: &Block<dyn Fn(*const c_void)>);
type SendCmdFn = unsafe extern "C" fn(cmd: u32, opts: *const c_void) -> u8;

const MR: &std::ffi::CStr = c"/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote";

fn sym(name: &std::ffi::CStr) -> Option<*mut c_void> {
    unsafe {
        let h = dlopen(MR.as_ptr(), 1);
        if h.is_null() {
            return None;
        }
        let s = dlsym(h, name.as_ptr());
        (!s.is_null()).then_some(s)
    }
}

pub fn run() -> String {
    autoreleasepool(|_| match std::env::var("PM_NP_MODE").as_deref() {
        Ok("send") => {
            let cmd = std::env::var("PM_NP_CMD").ok().and_then(|c| c.parse::<u32>().ok());
            let ok = match (cmd, sym(c"MRMediaRemoteSendCommand")) {
                (Some(c), Some(f)) => unsafe { std::mem::transmute::<*mut c_void, SendCmdFn>(f)(c, std::ptr::null()) != 0 },
                _ => false,
            };
            // The command goes out asynchronously; exiting right away can
            // drop it (browsers never saw it). Give it time to be delivered.
            if ok {
                unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.5, 0) };
            }
            format!("sent {}\n", ok as u8)
        }
        _ => get().unwrap_or_else(|| "none\n".to_string()),
    })
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn dict_get(d: &AnyObject, key: &str) -> Option<Retained<AnyObject>> {
    unsafe { msg_send![d, objectForKey: &*ns(&format!("kMRMediaRemoteNowPlayingInfo{key}"))] }
}

fn string_of(o: Option<&AnyObject>) -> String {
    let Some(o) = o else { return String::new() };
    let d: Retained<NSString> = unsafe { msg_send![o, description] };
    d.to_string()
}

/// One field of the output line: no tabs or newlines.
fn field(s: &str) -> String {
    s.split(['\t', '\n', '\r']).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

fn num(d: &AnyObject, key: &str) -> String {
    dict_get(d, key).map(|v| unsafe { msg_send![&*v, doubleValue] }).map(|v: f64| v.to_string()).unwrap_or_default()
}

fn get() -> Option<String> {
    let f = sym(c"MRMediaRemoteGetNowPlayingInfo")?;
    let get_info = unsafe { std::mem::transmute::<*mut c_void, GetInfoFn>(f) };
    let known_art = std::env::var("PM_NP_ART").unwrap_or_default();
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let block = RcBlock::new(move |dict: *const c_void| {
        let r = autoreleasepool(|_| {
            if dict.is_null() {
                return None;
            }
            let d = unsafe { &*(dict as *const AnyObject) };
            let s = |k: &str| field(&string_of(dict_get(d, k).as_deref()));
            let title = s("Title");
            if title.trim().is_empty() {
                return None;
            }
            let ts = dict_get(d, "Timestamp").map(|t| unsafe { msg_send![&*t, timeIntervalSince1970] }).map(|v: f64| v.to_string()).unwrap_or_default();
            let art = dict_get(d, "ArtworkData").map(|a| unsafe { (*(Retained::as_ptr(&a) as *const NSData)).to_vec() }).filter(|a| a.len() > 16).or_else(item_artwork);
            let (hash, b64) = match &art {
                Some(a) => {
                    let h = format!("{:016x}", fnv1a(a));
                    let b = if h == known_art { String::new() } else { base64(a) };
                    (h, b)
                }
                None => (String::new(), String::new()),
            };
            Some(format!(
                "np\t{}\t{title}\t{}\t{}\t{}\t{}\t{ts}\t{}\t{hash}\t{b64}\n",
                num(d, "PlaybackRate"),
                s("Artist"),
                s("Album"),
                num(d, "Duration"),
                num(d, "ElapsedTime"),
                field(&player_name()),
            ))
        });
        let _ = tx.send(r);
    });
    unsafe { get_info(dispatch_get_global_queue(0, 0), &block) };
    rx.recv_timeout(Duration::from_millis(1500)).ok().flatten()
}

/// Artwork of the current item, asked for separately (the info dictionary
/// usually leaves it out).
fn item_artwork() -> Option<Vec<u8>> {
    let cls = AnyClass::get(c"MRNowPlayingRequest")?;
    let (tx, rx) = mpsc::channel::<Option<Vec<u8>>>();
    let block = RcBlock::new(move |art: *mut AnyObject, _err: *mut AnyObject| {
        let bytes = autoreleasepool(|_| {
            if art.is_null() {
                return None;
            }
            let data: Option<Retained<NSData>> = unsafe { msg_send![&*art, imageData] };
            data.map(|d| d.to_vec())
        });
        let _ = tx.send(bytes);
    });
    unsafe {
        let req: Retained<AnyObject> = msg_send![msg_send![cls, alloc], init];
        let queue = &*(dispatch_get_global_queue(0, 0) as *const AnyObject);
        let _: () = msg_send![&*req, requestNowPlayingItemArtworkOnQueue: queue, completion: &*block];
    }
    rx.recv_timeout(Duration::from_millis(1500)).ok().flatten().filter(|a| a.len() > 16)
}

/// Display name of the now-playing app ("Brave Browser", "Music").
fn player_name() -> String {
    let Some(cls) = AnyClass::get(c"MRNowPlayingRequest") else { return String::new() };
    unsafe {
        let path: Option<Retained<AnyObject>> = msg_send![cls, localNowPlayingPlayerPath];
        let Some(path) = path else { return String::new() };
        let client: Option<Retained<AnyObject>> = msg_send![&*path, client];
        let Some(client) = client else { return String::new() };
        let name: Option<Retained<NSString>> = msg_send![&*client, displayName];
        name.map(|n| n.to_string()).unwrap_or_default()
    }
}

fn fnv1a(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf29ce484222325u64, |h, x| (h ^ *x as u64).wrapping_mul(0x100000001b3))
}

fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            s.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"hi!"), "aGkh");
        assert_eq!(base64(b"h"), "aA==");
    }

    #[test]
    fn fields_have_no_separators() {
        assert_eq!(field("a\tb\nc"), "a b c");
    }
}

