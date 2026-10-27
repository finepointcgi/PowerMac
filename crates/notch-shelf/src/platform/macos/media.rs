//! Now Playing on macOS, as a fallback chain:
//! 1. MediaRemote (private framework; since macOS 15.4 it only answers
//!    entitled processes, so it usually returns nothing for third-party
//!    apps — used whenever it does return data);
//! 2. the `pm-nowplaying` helper library (shipped in the app's Frameworks)
//!    loaded into `/usr/bin/perl`: an Apple process, so MediaRemote answers
//!    it — any player, browsers too, with artwork, and it takes commands;
//! 3. MediaRemote read from JavaScript for Automation (`osascript`, also an
//!    Apple process) when the helper is missing: no artwork;
//! 4. AppleScript for Music and Spotify (opt-in, only when the app is
//!    already running; macOS asks once per app for Automation access);
//! 5. no metadata: the controls post media keys.

use std::ffi::{c_char, c_void};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command as Proc, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Whether MediaRemote answered this process last time (then it also takes
/// our commands).
static MR_ANSWERS: AtomicBool = AtomicBool::new(false);

/// Ask MediaRemote; `None` when it gives nothing (no player, or not
/// entitled).
pub fn query_media_remote() -> Option<NowPlaying> {
    let r = query_media_remote_inner();
    MR_ANSWERS.store(r.is_some(), Ordering::Relaxed);
    r
}

fn query_media_remote_inner() -> Option<NowPlaying> {
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

/// Loads the helper (argv[0]) and calls its entry point as an XSUB.
const HELPER_LOADER: &str = r#"use DynaLoader; my $l = DynaLoader::dl_load_file($ARGV[0], 0) or die DynaLoader::dl_error(); my $s = DynaLoader::dl_find_symbol($l, "pm_np_main") or die DynaLoader::dl_error(); DynaLoader::dl_install_xsub("main::pm_np", $s); main::pm_np();"#;
const HELPER_NAME: &str = "libpm_nowplaying.dylib";
const PERL: &str = "/usr/bin/perl";

/// The helper library: `Contents/Frameworks` of the app bundle, or next to
/// the executable (cargo builds).
fn helper_path() -> Option<&'static PathBuf> {
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        let found = [dir.join("../Frameworks").join(HELPER_NAME), dir.join(HELPER_NAME)].into_iter().find_map(|p| p.canonicalize().ok().filter(|p| p.is_file()));
        match &found {
            Some(p) if std::path::Path::new(PERL).is_file() => log::info!("notch-shelf: now playing helper {}", p.display()),
            _ => log::info!("notch-shelf: no now playing helper (or no {PERL})"),
        }
        found.filter(|_| std::path::Path::new(PERL).is_file())
    })
    .as_ref()
}

fn run_helper(env: &[(&str, &str)]) -> Option<String> {
    let mut cmd = Proc::new(PERL);
    cmd.arg("-e").arg(HELPER_LOADER).arg(helper_path()?);
    for (k, v) in env {
        cmd.env(k, v);
    }
    run_with_timeout(cmd, Duration::from_secs(4))
}

/// What the helper reported.
pub enum HelperAnswer {
    /// Nothing is playing.
    Nothing,
    /// The current item; `art_hash` identifies its artwork, and `artwork`
    /// is only sent when that differs from the hash passed in.
    Item { np: NowPlaying, art_hash: String, artwork: Option<Vec<u8>> },
}

/// Ask the helper; `None` when it isn't installed or failed. `known_art`
/// is the hash of the artwork already held.
pub fn query_helper(now: f64, known_art: &str) -> Option<HelperAnswer> {
    parse_helper_output(&run_helper(&[("PM_NP_MODE", "get"), ("PM_NP_ART", known_art)])?, now)
}

fn parse_helper_output(out: &str, now: f64) -> Option<HelperAnswer> {
    let line = out.trim_end_matches(['\n', '\r']);
    if line == "none" {
        return Some(HelperAnswer::Nothing);
    }
    let f: Vec<&str> = line.strip_prefix("np\t")?.split('\t').collect();
    if f.len() < 10 {
        return None;
    }
    let Some(np) = parse_np_fields(&f[..8], now) else { return Some(HelperAnswer::Nothing) };
    let artwork = Some(f[9].trim()).filter(|b| !b.is_empty()).and_then(crate::widgets::xbar::base64_decode);
    Some(HelperAnswer::Item { np, art_hash: f[8].trim().to_string(), artwork })
}

/// Send a MediaRemote command through the helper; false if it couldn't.
fn helper_send(cmd: Command) -> bool {
    run_helper(&[("PM_NP_MODE", "send"), ("PM_NP_CMD", &cmd.mr_command().to_string())]).is_some_and(|o| o.trim() == "sent 1")
}

/// JXA: the current item as one tab-separated line — rate, title, artist,
/// album, duration, elapsed, sample time (unix), player name. Empty when
/// nothing is playing.
const JXA_SCRIPT: &str = r#"ObjC.import('Foundation');
$.NSBundle.bundleWithPath('/System/Library/PrivateFrameworks/MediaRemote.framework/').load;
const R = $.NSClassFromString('MRNowPlayingRequest');
const item = R.localNowPlayingItem;
const info = (item && !item.isNil()) ? item.nowPlayingInfo : null;
if (!info || info.isNil()) { ''; } else {
  const g = k => { const v = info.objectForKey('kMRMediaRemoteNowPlayingInfo' + k); return (v && !v.isNil()) ? v : null; };
  const str = k => { const v = g(k); return v ? String(ObjC.unwrap(v.description)).replace(/[\t\r\n]+/g, ' ') : ''; };
  const num = k => { const v = g(k); return v ? String(v.doubleValue) : ''; };
  const ts = g('Timestamp');
  const path = R.localNowPlayingPlayerPath;
  const app = (path && !path.isNil() && !path.client.isNil()) ? String(ObjC.unwrap(path.client.displayName) || '') : '';
  [num('PlaybackRate'), str('Title'), str('Artist'), str('Album'), num('Duration'), num('ElapsedTime'),
   ts ? String(ts.timeIntervalSince1970) : '', app].join('\t');
}"#;

fn osascript_js(script: &str, timeout: Duration) -> Option<String> {
    run_osascript(&["-l", "JavaScript", "-e", script], timeout)
}

/// Ask MediaRemote through `osascript` (works where the direct query is
/// refused).
pub fn query_jxa(now: f64) -> Option<NowPlaying> {
    parse_jxa_output(&osascript_js(JXA_SCRIPT, Duration::from_secs(3))?, now)
}

fn parse_jxa_output(out: &str, now: f64) -> Option<NowPlaying> {
    let f: Vec<&str> = out.trim_end_matches(['\n', '\r']).split('\t').collect();
    parse_np_fields(&f, now)
}

/// rate, title, artist, album, duration, elapsed, sampled-at, player (the
/// JXA script's and the helper's shared fields).
fn parse_np_fields(f: &[&str], now: f64) -> Option<NowPlaying> {
    if f.len() < 8 {
        return None;
    }
    let num = |s: &str| s.trim().parse::<f64>().ok().filter(|v| v.is_finite());
    let np = NowPlaying {
        title: f[1].trim().to_string(),
        artist: f[2].trim().to_string(),
        album: f[3].trim().to_string(),
        app: f[7].trim().to_string(),
        playing: num(f[0]).is_some_and(|r| r > 0.0),
        duration: num(f[4]).filter(|d| *d > 0.0),
        elapsed: num(f[5]).filter(|e| *e >= 0.0),
        sampled_at: num(f[6]).filter(|t| *t > 0.0).unwrap_or(now),
        artwork: None,
        source: MediaSource::System,
    };
    np.has_track().then_some(np)
}

/// Run an AppleScript with `osascript` (so a hung player can't block us
/// for long); stdout on success.
pub fn osascript(script: &str, timeout: Duration) -> Option<String> {
    run_osascript(&["-e", script], timeout)
}

fn run_osascript(args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = Proc::new("/usr/bin/osascript");
    cmd.args(args);
    run_with_timeout(cmd, timeout)
}

/// Run `cmd`, killing it after `timeout`; stdout on success.
fn run_with_timeout(mut cmd: Proc, timeout: Duration) -> Option<String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
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
            // MediaRemote ignores our own commands unless it answers us;
            // the helper's are taken.
            let ok = if MR_ANSWERS.load(Ordering::Relaxed) {
                media_remote().and_then(|mr| mr.send).is_some_and(|f| unsafe { f(cmd.mr_command(), std::ptr::null()) } != 0)
            } else {
                helper_send(cmd)
            };
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
    fn jxa_query_runs() {
        // Read-only; prints whatever is playing.
        assert!(osascript_js(JXA_SCRIPT, Duration::from_secs(5)).is_some());
        let _ = query_jxa(0.0);
    }

    #[test]
    fn parses_helper_output() {
        let HelperAnswer::Item { np, art_hash, artwork } = parse_helper_output("np\t1\tParanormal\tPurrple Cat\tEndless Forest\t178.2\t12\t1793060594.7\tBrave Browser\t51d2\taGVsbG8=\n", 5.0).unwrap() else { panic!() };
        assert!(np.playing && np.source == MediaSource::System);
        assert_eq!((np.title.as_str(), np.app.as_str(), np.duration, np.sampled_at), ("Paranormal", "Brave Browser", Some(178.2), 1793060594.7));
        assert_eq!((art_hash.as_str(), artwork.as_deref()), ("51d2", Some(&b"hello"[..])));
        let HelperAnswer::Item { artwork, .. } = parse_helper_output("np\t0\tT\t\t\t\t\t\t\t51d2\t\n", 5.0).unwrap() else { panic!() };
        assert!(artwork.is_none());
        assert!(matches!(parse_helper_output("none\n", 5.0), Some(HelperAnswer::Nothing)));
        assert!(parse_helper_output("error\n", 5.0).is_none());
        assert!(parse_helper_output("", 5.0).is_none());
    }

    #[test]
    fn parses_jxa_output() {
        let np = parse_jxa_output("1\tEndless Forest\tPurrple Cat\tEndless Forest\t157.8\t6.6\t1793049331.5\tBrave Browser\n", 5.0).unwrap();
        assert!(np.playing);
        assert_eq!((np.title.as_str(), np.artist.as_str(), np.app.as_str()), ("Endless Forest", "Purrple Cat", "Brave Browser"));
        assert_eq!((np.duration, np.elapsed, np.sampled_at), (Some(157.8), Some(6.6), 1793049331.5));
        let np = parse_jxa_output("0\tT\t\t\t\t\t\t\n", 5.0).unwrap();
        assert!(!np.playing && np.duration.is_none() && np.sampled_at == 5.0);
        assert!(parse_jxa_output("", 5.0).is_none());
        assert!(parse_jxa_output("1\t\t\t\t\t\t\t", 5.0).is_none());
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
