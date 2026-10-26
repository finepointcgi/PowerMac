//! Per-app volume on macOS 14.4+ with Core Audio process taps.
//!
//! For every app whose level is not 100 %:
//!
//! * **muted / 0 %** – a private process tap with `CATapMuted`: the app's
//!   audio no longer reaches the hardware; nobody reads the tap.
//! * **other levels** – a private tap with `CATapMutedWhenTapped` on the
//!   app's processes, plus a private aggregate device whose main sub-device
//!   is the current default output and whose tap list holds that tap
//!   (drift-compensated). Our IOProc on the aggregate copies the tap's
//!   stereo mix to the output streams scaled by the gain (up to 150 %, hard
//!   clipped). While our IOProc runs, the app's original output is muted, so
//!   only the scaled copy is heard; if we stop (or crash) the app is audible
//!   again at its normal level.
//!
//! Taps are rebuilt when the default output changes or the app's set of
//! audio processes changes (Chrome/Safari spawn helpers).
//!
//! Requirements: the audio-capture permission (TCC `kTCCServiceAudioCapture`,
//! Info.plist `NSAudioCaptureUsageDescription`). Without it the tap delivers
//! silence, so scaled taps are refused while the permission is denied.

use super::ffi::*;
use super::{default_output_uid, props, tcc, AGGREGATE_UID_PREFIX};
use crate::{AppLevel, AudioApp, CapturePermission};
use anyhow::{anyhow, Result};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2::rc::{autoreleasepool, Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::{NSArray, NSBundle, NSNumber, NSString, NSUUID};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;

const CA_TAP_MUTED: isize = 1;
const CA_TAP_MUTED_WHEN_TAPPED: isize = 2;

type CreateTapFn = unsafe extern "C" fn(*mut c_void, *mut AudioObjectID) -> OSStatus;
type DestroyTapFn = unsafe extern "C" fn(AudioObjectID) -> OSStatus;

fn tap_fns() -> Option<(CreateTapFn, DestroyTapFn)> {
    static FNS: OnceLock<(usize, usize)> = OnceLock::new();
    let (c, d) = *FNS.get_or_init(|| {
        let lib = c"/System/Library/Frameworks/CoreAudio.framework/CoreAudio";
        (
            tcc::sym(lib, c"AudioHardwareCreateProcessTap") as usize,
            tcc::sym(lib, c"AudioHardwareDestroyProcessTap") as usize,
        )
    });
    if c == 0 || d == 0 {
        return None;
    }
    unsafe { Some((std::mem::transmute::<usize, CreateTapFn>(c), std::mem::transmute::<usize, DestroyTapFn>(d))) }
}

extern "C" {
    fn sysctlbyname(name: *const std::ffi::c_char, oldp: *mut c_void, oldlenp: *mut usize, newp: *mut c_void, newlen: usize) -> i32;
}

/// (major, minor) of macOS.
pub(crate) fn os_version() -> (u32, u32) {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    let r = unsafe { sysctlbyname(c"kern.osproductversion".as_ptr(), buf.as_mut_ptr() as *mut c_void, &mut len, std::ptr::null_mut(), 0) };
    if r != 0 {
        return (0, 0);
    }
    let s = CStr::from_bytes_until_nul(&buf).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
    let mut it = s.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// macOS 14.4+ (process taps shipped in 14.2, usable aggregate tap lists
/// and the permission prompt in 14.4) and the API is present.
pub(crate) fn supported() -> bool {
    static S: OnceLock<bool> = OnceLock::new();
    *S.get_or_init(|| os_version() >= (14, 4) && tap_fns().is_some() && AnyClass::get(c"CATapDescription").is_some())
}

/// One CoreAudio process object.
#[derive(Debug, Clone)]
pub(crate) struct ProcInfo {
    pub obj: AudioObjectID,
    pub pid: i32,
    pub app_id: String,
    pub name: String,
    pub playing: bool,
}

/// Shared with the IOProc.
struct IoState {
    gain: AtomicU32,
    /// Peak of the tapped input in the last callback (f32 bits), for diagnostics.
    peak: AtomicU32,
    callbacks: AtomicU64,
}

enum TapMode {
    Muted,
    Scaled { aggregate: AudioObjectID, proc_id: AudioDeviceIOProcID, state: *mut IoState },
}

struct AppTap {
    objs: Vec<AudioObjectID>,
    output_uid: String,
    tap_id: AudioObjectID,
    mode: TapMode,
}

// The raw pointers are owned by the tap and only shared with our IOProc.
unsafe impl Send for AppTap {}

#[derive(Default)]
pub(crate) struct TapManager {
    taps: HashMap<String, AppTap>,
    app_cache: HashMap<i32, (String, String)>,
}

impl TapManager {
    /// Every CoreAudio client process, resolved to its app.
    pub(crate) fn processes(&mut self) -> Vec<ProcInfo> {
        let objs = props::get_ids(kAudioObjectSystemObject, AudioObjectPropertyAddress::global(kAudioHardwarePropertyProcessObjectList))
            .unwrap_or_default();
        let mut out = Vec::new();
        let mut live = HashSet::new();
        for obj in objs {
            let Ok(pid) = props::get::<i32>(obj, AudioObjectPropertyAddress::global(kAudioProcessPropertyPID)) else { continue };
            if pid <= 0 {
                continue;
            }
            live.insert(pid);
            let playing = props::get::<u32>(obj, AudioObjectPropertyAddress::global(kAudioProcessPropertyIsRunningOutput)).unwrap_or(0) != 0;
            let (app_id, name) = match self.app_cache.get(&pid) {
                Some(v) => v.clone(),
                None => {
                    let bid = props::get_string(obj, AudioObjectPropertyAddress::global(kAudioProcessPropertyBundleID)).unwrap_or_default();
                    let v = resolve_app(pid, &bid);
                    self.app_cache.insert(pid, v.clone());
                    v
                }
            };
            out.push(ProcInfo { obj, pid, app_id, name, playing });
        }
        self.app_cache.retain(|pid, _| live.contains(pid));
        out
    }

    pub(crate) fn apps(&mut self, levels: &BTreeMap<String, AppLevel>) -> Vec<AudioApp> {
        let mut apps: BTreeMap<String, AudioApp> = BTreeMap::new();
        for p in self.processes() {
            let a = apps.entry(p.app_id.clone()).or_insert_with(|| {
                let l = levels.get(&p.app_id).copied().unwrap_or_default();
                AudioApp {
                    pid: p.pid,
                    pids: Vec::new(),
                    bundle_id: p.app_id.clone(),
                    name: p.name.clone(),
                    volume: l.volume,
                    muted: l.muted,
                    playing: false,
                }
            });
            if !a.pids.contains(&p.pid) {
                a.pids.push(p.pid);
            }
            a.playing |= p.playing;
        }
        apps.into_values().filter(|a| a.playing || levels.contains_key(&a.bundle_id)).collect()
    }

    fn destroy(&mut self, app: &str) {
        if let Some(t) = self.taps.remove(app) {
            destroy_tap(t);
        }
    }

    pub(crate) fn apply(&mut self, levels: &BTreeMap<String, AppLevel>, enabled: bool) -> Result<()> {
        if !enabled || !supported() {
            for app in self.taps.keys().cloned().collect::<Vec<_>>() {
                self.destroy(&app);
            }
            return Ok(());
        }
        let wanted: BTreeMap<&String, &AppLevel> = levels.iter().filter(|(_, l)| !l.is_neutral()).collect();
        if wanted.is_empty() && self.taps.is_empty() {
            return Ok(());
        }
        let procs = self.processes();
        let output = default_output_uid().filter(|u| !u.starts_with(AGGREGATE_UID_PREFIX));
        let mut errors = Vec::new();

        let stale: Vec<String> = self.taps.keys().filter(|k| !wanted.contains_key(k)).cloned().collect();
        for app in stale {
            self.destroy(&app);
        }

        for (app, level) in wanted {
            let mut objs: Vec<AudioObjectID> = procs.iter().filter(|p| &p.app_id == app).map(|p| p.obj).collect();
            objs.sort_unstable();
            objs.dedup();
            if objs.is_empty() {
                self.destroy(app);
                continue;
            }
            let muted = level.muted || level.volume < 0.005;
            let out_uid = output.clone().unwrap_or_default();
            if let Some(t) = self.taps.get(app) {
                let same_shape = t.objs == objs
                    && match t.mode {
                        TapMode::Muted => muted,
                        TapMode::Scaled { .. } => !muted && t.output_uid == out_uid,
                    };
                if same_shape {
                    if let TapMode::Scaled { state, .. } = t.mode {
                        unsafe { (*state).gain.store(level.volume.to_bits(), Ordering::Relaxed) };
                    }
                    continue;
                }
                self.destroy(app);
            }
            let res = if muted {
                create_tap(&objs, CA_TAP_MUTED).map(|tap_id| AppTap { objs: objs.clone(), output_uid: out_uid, tap_id, mode: TapMode::Muted })
            } else if output.is_none() {
                Err(anyhow!("no default output device"))
            } else if super::tcc::preflight() == CapturePermission::Denied {
                Err(anyhow!("audio capture permission denied (System Settings → Privacy & Security → Screen & System Audio Recording)"))
            } else {
                create_scaled(&objs, &out_uid, level.volume)
            };
            match res {
                Ok(t) => {
                    log::info!("audio-control: per-app tap for {app}: {:.0}%{}", level.volume * 100.0, if muted { " (muted)" } else { "" });
                    self.taps.insert(app.clone(), t);
                }
                Err(e) => errors.push(format!("{app}: {e:#}")),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(errors.join("; ")))
        }
    }

    pub(crate) fn diagnostics(&self) -> Vec<String> {
        self.taps
            .iter()
            .map(|(app, t)| match t.mode {
                TapMode::Muted => format!("{app}: muted tap {} on {} process(es)", t.tap_id, t.objs.len()),
                TapMode::Scaled { aggregate, state, .. } => unsafe {
                    format!(
                        "{app}: tap {} → aggregate {aggregate} on {}, gain {:.2}, {} callbacks, last input peak {:.5}",
                        t.tap_id,
                        t.output_uid,
                        f32::from_bits((*state).gain.load(Ordering::Relaxed)),
                        (*state).callbacks.load(Ordering::Relaxed),
                        f32::from_bits((*state).peak.load(Ordering::Relaxed)),
                    )
                },
            })
            .collect()
    }
}

impl Drop for TapManager {
    fn drop(&mut self) {
        for (_, t) in self.taps.drain() {
            destroy_tap(t);
        }
    }
}

/// pid → (app id, display name). Helpers fold into their outermost `.app`.
fn resolve_app(pid: i32, coreaudio_bundle_id: &str) -> (String, String) {
    let mut buf = vec![0u8; 4096];
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    let path = if n > 0 { String::from_utf8_lossy(&buf[..n as usize]).into_owned() } else { String::new() };
    if let Some(idx) = path.find(".app/") {
        let app_path = &path[..idx + 4];
        let stem = app_path.rsplit('/').next().unwrap_or(app_path).trim_end_matches(".app").to_string();
        let (bid, name) = autoreleasepool(|_| {
            let Some(b) = NSBundle::bundleWithPath(&NSString::from_str(app_path)) else { return (None, None) };
            let bid = b.bundleIdentifier().map(|s| s.to_string());
            let name = ["CFBundleDisplayName", "CFBundleName"].iter().find_map(|k| {
                b.objectForInfoDictionaryKey(&NSString::from_str(k))
                    .and_then(|o| o.downcast::<NSString>().ok())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
            });
            (bid, name)
        });
        let id = bid.or_else(|| (!coreaudio_bundle_id.is_empty()).then(|| coreaudio_bundle_id.to_string())).unwrap_or_else(|| stem.clone());
        return (id, name.unwrap_or(stem));
    }
    let exe = path.rsplit('/').next().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| format!("pid {pid}"));
    let id = if coreaudio_bundle_id.is_empty() { exe.clone() } else { coreaudio_bundle_id.to_string() };
    (id, exe)
}

fn create_tap(objs: &[AudioObjectID], mute_behavior: isize) -> Result<AudioObjectID> {
    let (create, _) = tap_fns().ok_or_else(|| anyhow!("process taps need macOS 14.4"))?;
    let cls = AnyClass::get(c"CATapDescription").ok_or_else(|| anyhow!("CATapDescription is unavailable"))?;
    autoreleasepool(|_| unsafe {
        let nums: Vec<Retained<NSNumber>> = objs.iter().map(|o| NSNumber::new_u32(*o)).collect();
        let arr = NSArray::from_retained_slice(&nums);
        let alloc: Allocated<AnyObject> = msg_send![cls, alloc];
        let desc: Option<Retained<AnyObject>> = msg_send![alloc, initStereoMixdownOfProcesses: &*arr];
        let desc = desc.ok_or_else(|| anyhow!("CATapDescription init failed"))?;
        let name = NSString::from_str("PowerMac per-app volume");
        let _: () = msg_send![&*desc, setName: &*name];
        let _: () = msg_send![&*desc, setPrivate: true];
        let _: () = msg_send![&*desc, setMuteBehavior: mute_behavior];
        let uuid = NSUUID::new();
        let _: () = msg_send![&*desc, setUUID: &*uuid];
        let mut tap_id: AudioObjectID = 0;
        check(create(Retained::as_ptr(&desc) as *mut c_void, &mut tap_id), "AudioHardwareCreateProcessTap")?;
        if tap_id == 0 {
            return Err(anyhow!("AudioHardwareCreateProcessTap returned no tap"));
        }
        Ok(tap_id)
    })
}

fn destroy_tap_object(tap_id: AudioObjectID) {
    if let Some((_, destroy)) = tap_fns() {
        let st = unsafe { destroy(tap_id) };
        if st != 0 {
            log::warn!("audio-control: AudioHardwareDestroyProcessTap: {}", status_str(st));
        }
    }
}

fn cfs(s: &str) -> CFType {
    CFString::new(s).as_CFType()
}

fn create_aggregate(output_uid: &str, tap_uid: &str) -> Result<AudioObjectID> {
    let uid = format!("{AGGREGATE_UID_PREFIX}{}", autoreleasepool(|_| NSUUID::new().UUIDString().to_string()));
    let sub = CFDictionary::from_CFType_pairs(&[(cfs("uid"), cfs(output_uid))]);
    let tap = CFDictionary::from_CFType_pairs(&[(cfs("uid"), cfs(tap_uid)), (cfs("drift"), CFNumber::from(1i32).as_CFType())]);
    let subs = CFArray::from_CFTypes(&[sub.as_CFType()]);
    let taps = CFArray::from_CFTypes(&[tap.as_CFType()]);
    let desc = CFDictionary::from_CFType_pairs(&[
        (cfs("uid"), cfs(&uid)),
        (cfs("name"), cfs("PowerMac App Volume")),
        (cfs("master"), cfs(output_uid)),
        (cfs("clock"), cfs(output_uid)),
        (cfs("private"), CFNumber::from(1i32).as_CFType()),
        (cfs("stacked"), CFNumber::from(0i32).as_CFType()),
        (cfs("tapautostart"), CFNumber::from(1i32).as_CFType()),
        (cfs("subdevices"), subs.as_CFType()),
        (cfs("taps"), taps.as_CFType()),
    ]);
    let mut id: AudioObjectID = 0;
    check(unsafe { AudioHardwareCreateAggregateDevice(desc.as_concrete_TypeRef(), &mut id) }, "AudioHardwareCreateAggregateDevice")?;
    Ok(id)
}

fn is_float32(dev: AudioObjectID, scope: u32) -> bool {
    let streams = props::get_ids(dev, AudioObjectPropertyAddress::new(kAudioDevicePropertyStreams, scope, kAudioObjectPropertyElementMain))
        .unwrap_or_default();
    !streams.is_empty()
        && streams.iter().all(|s| {
            props::get::<AudioStreamBasicDescription>(*s, AudioObjectPropertyAddress::global(kAudioStreamPropertyVirtualFormat))
                .map(|f| f.mFormatID == kAudioFormatLinearPCM && f.mFormatFlags & kAudioFormatFlagIsFloat != 0 && f.mBitsPerChannel == 32)
                .unwrap_or(false)
        })
}

fn create_scaled(objs: &[AudioObjectID], output_uid: &str, gain: f32) -> Result<AppTap> {
    let tap_id = create_tap(objs, CA_TAP_MUTED_WHEN_TAPPED)?;
    let fail = |e: anyhow::Error, agg: AudioObjectID| {
        if agg != 0 {
            unsafe { AudioHardwareDestroyAggregateDevice(agg) };
        }
        destroy_tap_object(tap_id);
        e
    };
    let tap_uid = props::get_string(tap_id, AudioObjectPropertyAddress::global(kAudioTapPropertyUID)).map_err(|e| fail(e, 0))?;
    let agg = create_aggregate(output_uid, &tap_uid).map_err(|e| fail(e, 0))?;
    if !is_float32(agg, kAudioObjectPropertyScopeOutput) || !is_float32(agg, kAudioObjectPropertyScopeInput) {
        return Err(fail(anyhow!("the output device does not use 32-bit float streams"), agg));
    }
    let state = Box::into_raw(Box::new(IoState {
        gain: AtomicU32::new(gain.to_bits()),
        peak: AtomicU32::new(0),
        callbacks: AtomicU64::new(0),
    }));
    let mut proc_id: AudioDeviceIOProcID = std::ptr::null_mut();
    let st = unsafe { AudioDeviceCreateIOProcID(agg, io_proc, state as *mut c_void, &mut proc_id) };
    if st != 0 {
        unsafe { drop(Box::from_raw(state)) };
        return Err(fail(anyhow!("AudioDeviceCreateIOProcID: {}", status_str(st)), agg));
    }
    let st = unsafe { AudioDeviceStart(agg, proc_id) };
    if st != 0 {
        unsafe {
            AudioDeviceDestroyIOProcID(agg, proc_id);
            drop(Box::from_raw(state));
        }
        return Err(fail(anyhow!("AudioDeviceStart: {}", status_str(st)), agg));
    }
    Ok(AppTap {
        objs: objs.to_vec(),
        output_uid: output_uid.to_string(),
        tap_id,
        mode: TapMode::Scaled { aggregate: agg, proc_id, state },
    })
}

fn destroy_tap(t: AppTap) {
    if let TapMode::Scaled { aggregate, proc_id, state } = t.mode {
        unsafe {
            AudioDeviceStop(aggregate, proc_id);
            AudioDeviceDestroyIOProcID(aggregate, proc_id);
            AudioHardwareDestroyAggregateDevice(aggregate);
            drop(Box::from_raw(state));
        }
    }
    destroy_tap_object(t.tap_id);
}

/// Copy the tap (the last input stream: the aggregate lists the sub-device's
/// own inputs first) to every output stream, scaled.
unsafe extern "C" fn io_proc(
    _device: AudioObjectID,
    _now: *const AudioTimeStamp,
    input: *const AudioBufferList,
    _input_time: *const AudioTimeStamp,
    output: *mut AudioBufferList,
    _output_time: *const AudioTimeStamp,
    data: *mut c_void,
) -> OSStatus {
    let st = &*(data as *const IoState);
    st.callbacks.fetch_add(1, Ordering::Relaxed);
    let gain = f32::from_bits(st.gain.load(Ordering::Relaxed));
    let outs = AudioBufferList::buffers_mut(output);
    let ins = AudioBufferList::buffers(input);
    let tap = ins.last().filter(|b| !b.mData.is_null() && b.mNumberChannels > 0);
    let (src, in_ch): (&[f32], usize) = match tap {
        Some(b) => (std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4), b.mNumberChannels as usize),
        None => (&[], 1),
    };
    let frames = src.len() / in_ch;
    let mut peak = 0f32;
    for s in src {
        peak = peak.max(s.abs());
    }
    st.peak.store(peak.to_bits(), Ordering::Relaxed);

    // Global channel index across output buffers; the tap's L/R go to the
    // first two output channels.
    let mut base = 0usize;
    for ob in outs.iter_mut() {
        if ob.mData.is_null() || ob.mNumberChannels == 0 {
            continue;
        }
        let n = ob.mNumberChannels as usize;
        let dst = std::slice::from_raw_parts_mut(ob.mData as *mut f32, ob.mDataByteSize as usize / 4);
        let out_frames = dst.len() / n;
        for f in 0..out_frames {
            for c in 0..n {
                let gc = base + c;
                dst[f * n + c] = if f < frames && gc < 2 { (src[f * in_ch + gc.min(in_ch - 1)] * gain).clamp(-1.0, 1.0) } else { 0.0 };
            }
        }
        base += n;
    }
    0
}
