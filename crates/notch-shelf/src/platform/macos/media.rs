//! Now Playing on macOS, as a fallback chain:
//! 1. MediaRemote (private framework; since macOS 15.4 it only answers
//!    entitled processes, so it usually returns nothing for third-party
//!    apps — used whenever it does return data);
//! 2. AppleScript for Music and Spotify (opt-in, only when the app is
//!    already running; macOS asks once per app for Automation access);
//! 3. no metadata: the controls post media keys.

use std::ffi::{c_char, c_void};
use std::io::Read;
use std::process::{Command as Proc, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use block2::{Block, RcBlock};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::msg_send;
use objc2_foundation::NSData;

use super::ffi::ns;
use super::rt;
use crate::widgets::media::{self as m, Command};
use crate::widgets::{MediaSource, NowPlaying};

extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dispatch_get_global_queue(id: isize, flags: usize) -> *const c_void;
}

type GetInfoFn = unsafe extern "C" fn(queue: *const c_void, block: &Block<dyn Fn(*const c_void)>);
type SendCmdFn = unsafe extern "C" fn(cmd: u32, opts: *const c_void) -> u8;

struct MediaRemote {
    get_info: GetInfoFn,
    send: Option<SendCmdFn>,
}

fn media_remote() -> Option<&'static MediaRemote> {
    use std::sync::OnceLock;
    static MR: OnceLock<Option<MediaRemote>> = OnceLock::new();
    MR.get_or_init(|| unsafe {
        let h = dlopen(c"/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote".as_ptr(), 1);
        if h.is_null() {
            return None;
        }
        let gi = dlsym(h, c"MRMediaRemoteGetNowPlayingInfo".as_ptr());
        let sc = dlsym(h, c"MRMediaRemoteSendCommand".as_ptr());
        if gi.is_null() {
            return None;
        }
        Some(MediaRemote {
            get_info: std::mem::transmute::<*mut c_void, GetInfoFn>(gi),
            send: (!sc.is_null()).then(|| std::mem::transmute::<*mut c_void, SendCmdFn>(sc)),
        })
    })
    .as_ref()
}

fn dict_get(d: &AnyObject, key: &str) -> Option<Retained<AnyObject>> {
    unsafe { msg_send![d, objectForKey: &*ns(&format!("kMRMediaRemoteNowPlayingInfo{key}"))] }
}

fn dict_f64(d: &AnyObject, key: &str) -> Option<f64> {
    dict_get(d, key).map(|v| unsafe { msg_send![&*v, doubleValue] })
}

/// Ask MediaRemote; `None` when it gives nothing (no player, or not
/// entitled).
pub fn query_media_remote() -> Option<NowPlaying> {
    let mr = media_remote()?;
    let (tx, rx) = mpsc::channel::<Option<NowPlaying>>();
    autoreleasepool(|_| unsafe {
        let block = RcBlock::new(move |dict: *const c_void| {
            let r = autoreleasepool(|_| {
                if dict.is_null() {
                    return None;
                }
                let d = &*(dict as *const AnyObject);
                let s = |k: &str| rt::string_of(dict_get(d, k).as_deref());
                let title = s("Title");
                if title.trim().is_empty() {
                    return None;
                }
                let rate = dict_f64(d, "PlaybackRate").unwrap_or(0.0);
                let ts = rt::unix_of_date(dict_get(d, "Timestamp").as_deref()).unwrap_or_else(crate::widgets::unix_now);
                let art = dict_get(d, "ArtworkData").map(|a| Arc::new((*(Retained::as_ptr(&a) as *const NSData)).to_vec()));
                Some(NowPlaying {
                    title,
                    artist: s("Artist"),
                    album: s("Album"),
                    app: String::new(),
                    playing: rate > 0.0,
                    duration: dict_f64(d, "Duration").filter(|v| *v > 0.0),
                    elapsed: dict_f64(d, "ElapsedTime"),
                    sampled_at: ts,
                    artwork: art.filter(|a| !a.is_empty()),
                    source: MediaSource::System,
                })
            });
            let _ = tx.send(r);
        });
        (mr.get_info)(dispatch_get_global_queue(0, 0), &block);
    });
    rx.recv_timeout(Duration::from_millis(1200)).ok().flatten()
}

/// Run an AppleScript with `osascript` (so a hung player can't block us
/// for long); stdout on success.
pub fn osascript(script: &str, timeout: Duration) -> Option<String> {
    let mut child = Proc::new("/usr/bin/osascript").arg("-e").arg(script).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                return st.success().then_some(out);
            }
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Current track of Music / Spotify (only those already running). Returns
/// the track and, for Spotify, its artwork URL.
pub fn query_applescript(now: f64) -> Option<(NowPlaying, Option<String>)> {
    let mut cands = vec![];
    for (id, script, src) in [(m::MUSIC_ID, m::MUSIC_SCRIPT, MediaSource::Music), (m::SPOTIFY_ID, m::SPOTIFY_SCRIPT, MediaSource::Spotify)] {
        if !rt::app_running(id) {
            continue;
        }
        if let Some(out) = osascript(script, Duration::from_secs(3)) {
            if let Some(r) = m::parse_script_output(&out, src, now) {
                cands.push(r);
            }
        }
    }
    let i = cands.iter().position(|(n, _)| n.playing).unwrap_or(0);
    (!cands.is_empty()).then(|| cands.swap_remove(i))
}

/// Artwork bytes for a track found through AppleScript.
pub fn applescript_artwork(np: &NowPlaying, url: Option<&str>) -> Option<Vec<u8>> {
    match np.source {
        MediaSource::Spotify => rt::http_get(url?, 10.0).ok(),
        MediaSource::Music => {
            let path = std::env::temp_dir().join(format!("notch-shelf-artwork-{}.dat", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let script = m::MUSIC_ARTWORK_SCRIPT.replace("{path}", &path.to_string_lossy().replace('"', ""));
            let ok = osascript(&script, Duration::from_secs(4)).is_some_and(|o| o.trim() == "ok");
            let bytes = if ok { std::fs::read(&path).ok() } else { None };
            let _ = std::fs::remove_file(&path);
            bytes.filter(|b| b.len() > 16)
        }
        _ => None,
    }
}

/// Send a transport command through the source the data came from.
pub fn send(cmd: Command, source: MediaSource) {
    match source {
        MediaSource::Music | MediaSource::Spotify => {
            let id = if source == MediaSource::Music { m::MUSIC_ID } else { m::SPOTIFY_ID };
            if osascript(&format!("tell application id \"{id}\" to {}", cmd.verb()), Duration::from_secs(3)).is_none() {
                super::ffi::dispatch_main(move || rt::post_media_key(cmd.nx_key()));
            }
        }
        MediaSource::System => {
            let ok = media_remote().and_then(|mr| mr.send).is_some_and(|f| unsafe { f(cmd.mr_command(), std::ptr::null()) } != 0);
            if !ok {
                super::ffi::dispatch_main(move || rt::post_media_key(cmd.nx_key()));
            }
        }
        MediaSource::MediaKeys => super::ffi::dispatch_main(move || rt::post_media_key(cmd.nx_key())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_remote_loads_and_answers_or_declines() {
        // Read-only: asks for the now-playing info (no playback control).
        assert!(media_remote().is_some());
        let _ = query_media_remote();
    }

    #[test]
    fn scripts_compile() {
        // osacompile reads Music's dictionary; nothing is sent to the app,
        // so no Automation prompt.
        let dir = std::env::temp_dir().join(format!("notch-osa-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut scripts = vec![("music", m::MUSIC_SCRIPT.to_string()), ("art", m::MUSIC_ARTWORK_SCRIPT.replace("{path}", "/tmp/x"))];
        if std::path::Path::new("/Applications/Spotify.app").exists() {
            scripts.push(("spotify", m::SPOTIFY_SCRIPT.to_string()));
        }
        for (name, src) in scripts {
            let out = dir.join(format!("{name}.scpt"));
            let st = Proc::new("/usr/bin/osacompile").arg("-e").arg(&src).arg("-o").arg(&out).stderr(Stdio::piped()).output().unwrap();
            assert!(st.status.success(), "{name}: {}", String::from_utf8_lossy(&st.stderr));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(osascript("return 1 + 1", Duration::from_secs(5)).as_deref().map(str::trim), Some("2"));
    }
}
