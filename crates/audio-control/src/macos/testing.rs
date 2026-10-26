//! Helpers for `examples/audio_probe.rs` (not part of the API).

use super::ffi::*;
use super::{default_output_uid, id_for_uid};
use anyhow::{anyhow, Result};
use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A public aggregate device wrapping the current default output, to
/// simulate "a device appeared". Destroyed on drop.
pub struct TestDevice {
    pub id: AudioObjectID,
    pub uid: String,
}

impl TestDevice {
    pub fn create(name: &str) -> Result<TestDevice> {
        let out = default_output_uid().ok_or_else(|| anyhow!("no default output"))?;
        let uid = format!("powermac-probe-{}", std::process::id());
        let k = |s: &str| CFString::new(s).as_CFType();
        let sub = CFDictionary::from_CFType_pairs(&[(k("uid"), k(&out))]);
        let subs = CFArray::from_CFTypes(&[sub.as_CFType()]);
        let desc = CFDictionary::from_CFType_pairs(&[
            (k("uid"), k(&uid)),
            (k("name"), k(name)),
            (k("master"), k(&out)),
            (k("private"), CFNumber::from(0i32).as_CFType()),
            (k("stacked"), CFNumber::from(0i32).as_CFType()),
            (k("subdevices"), subs.as_CFType()),
        ]);
        let mut id = 0;
        check(unsafe { AudioHardwareCreateAggregateDevice(desc.as_concrete_TypeRef(), &mut id) }, "AudioHardwareCreateAggregateDevice")?;
        // Wait until it is listed.
        for _ in 0..50 {
            if id_for_uid(&uid).is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(TestDevice { id, uid })
    }
}

impl Drop for TestDevice {
    fn drop(&mut self) {
        unsafe { AudioHardwareDestroyAggregateDevice(self.id) };
    }
}

struct Tone {
    phase: f64,
    step: f64,
    amp: f32,
    frames: AtomicU64,
}

unsafe extern "C" fn tone_proc(
    _d: AudioObjectID,
    _n: *const AudioTimeStamp,
    _i: *const AudioBufferList,
    _it: *const AudioTimeStamp,
    output: *mut AudioBufferList,
    _ot: *const AudioTimeStamp,
    data: *mut c_void,
) -> OSStatus {
    let t = &mut *(data as *mut Tone);
    let outs = AudioBufferList::buffers_mut(output);
    let mut frames_done = 0;
    for (bi, ob) in outs.iter_mut().enumerate() {
        if ob.mData.is_null() || ob.mNumberChannels == 0 {
            continue;
        }
        let n = ob.mNumberChannels as usize;
        let dst = std::slice::from_raw_parts_mut(ob.mData as *mut f32, ob.mDataByteSize as usize / 4);
        let frames = dst.len() / n;
        let mut ph = t.phase;
        for f in 0..frames {
            let v = (ph.sin() as f32) * t.amp;
            ph += t.step;
            for c in 0..n {
                dst[f * n + c] = v;
            }
        }
        if bi == 0 {
            frames_done = frames;
        }
    }
    t.phase = (t.phase + t.step * frames_done as f64) % std::f64::consts::TAU;
    t.frames.fetch_add(frames_done as u64, Ordering::Relaxed);
    0
}

/// Play a sine of `amp` (linear, e.g. 0.0005 ≈ -66 dBFS) on the default
/// output from this process while `during` runs. Returns `during`'s result
/// and the frames rendered.
pub fn with_quiet_tone<R>(amp: f32, during: impl FnOnce() -> R) -> Result<(R, u64)> {
    let out = default_output_uid().ok_or_else(|| anyhow!("no default output"))?;
    let dev = id_for_uid(&out).ok_or_else(|| anyhow!("default output vanished"))?;
    let tone = Box::into_raw(Box::new(Tone { phase: 0.0, step: std::f64::consts::TAU * 440.0 / 48_000.0, amp, frames: AtomicU64::new(0) }));
    let mut pid: AudioDeviceIOProcID = std::ptr::null_mut();
    unsafe {
        if let Err(e) = check(AudioDeviceCreateIOProcID(dev, tone_proc, tone as *mut c_void, &mut pid), "AudioDeviceCreateIOProcID") {
            drop(Box::from_raw(tone));
            return Err(e);
        }
        if let Err(e) = check(AudioDeviceStart(dev, pid), "AudioDeviceStart") {
            AudioDeviceDestroyIOProcID(dev, pid);
            drop(Box::from_raw(tone));
            return Err(e);
        }
    }
    let r = during();
    unsafe {
        AudioDeviceStop(dev, pid);
        AudioDeviceDestroyIOProcID(dev, pid);
        let t = Box::from_raw(tone);
        Ok((r, t.frames.load(Ordering::Relaxed)))
    }
}

/// Default output / input device uids straight from CoreAudio.
pub fn default_uid(kind: crate::DeviceKind) -> Option<String> {
    let id = super::default_device_id(kind);
    (id != kAudioObjectUnknown).then(|| super::device_uid(id)).flatten()
}

pub fn os_version() -> (u32, u32) {
    super::tap::os_version()
}
