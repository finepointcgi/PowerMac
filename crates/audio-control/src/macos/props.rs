//! Typed AudioObject property access.

use super::ffi::*;
use anyhow::Result;
use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::mem::{size_of, MaybeUninit};

pub fn has(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> bool {
    unsafe { AudioObjectHasProperty(id, &addr) != 0 }
}

pub fn settable(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> bool {
    let mut s = 0u8;
    unsafe { AudioObjectIsPropertySettable(id, &addr, &mut s) == 0 && s != 0 }
}

/// Fixed-size POD property.
pub fn get<T: Copy>(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> Result<T> {
    let mut v = MaybeUninit::<T>::zeroed();
    let mut size = size_of::<T>() as u32;
    let st = unsafe { AudioObjectGetPropertyData(id, &addr, 0, std::ptr::null(), &mut size, v.as_mut_ptr() as *mut c_void) };
    check(st, "AudioObjectGetPropertyData")?;
    Ok(unsafe { v.assume_init() })
}

pub fn set<T: Copy>(id: AudioObjectID, addr: AudioObjectPropertyAddress, v: T) -> Result<()> {
    let st = unsafe {
        AudioObjectSetPropertyData(id, &addr, 0, std::ptr::null(), size_of::<T>() as u32, &v as *const T as *const c_void)
    };
    check(st, "AudioObjectSetPropertyData")
}

/// Variable-size property as raw, 8-byte aligned bytes.
pub fn get_bytes(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> Result<Vec<u64>> {
    let mut size = 0u32;
    let st = unsafe { AudioObjectGetPropertyDataSize(id, &addr, 0, std::ptr::null(), &mut size) };
    check(st, "AudioObjectGetPropertyDataSize")?;
    let mut buf = vec![0u64; (size as usize).div_ceil(8).max(1)];
    let st = unsafe { AudioObjectGetPropertyData(id, &addr, 0, std::ptr::null(), &mut size, buf.as_mut_ptr() as *mut c_void) };
    check(st, "AudioObjectGetPropertyData")?;
    buf.truncate((size as usize).div_ceil(8).max(1));
    Ok(buf)
}

/// Array of u32 (object ids).
pub fn get_ids(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> Result<Vec<u32>> {
    let mut size = 0u32;
    let st = unsafe { AudioObjectGetPropertyDataSize(id, &addr, 0, std::ptr::null(), &mut size) };
    check(st, "AudioObjectGetPropertyDataSize")?;
    let mut v = vec![0u32; size as usize / 4];
    if v.is_empty() {
        return Ok(v);
    }
    let st = unsafe { AudioObjectGetPropertyData(id, &addr, 0, std::ptr::null(), &mut size, v.as_mut_ptr() as *mut c_void) };
    check(st, "AudioObjectGetPropertyData")?;
    v.truncate(size as usize / 4);
    Ok(v)
}

/// CFString property (returned +1 by CoreAudio).
pub fn get_string(id: AudioObjectID, addr: AudioObjectPropertyAddress) -> Result<String> {
    let r: CFStringRef = get(id, addr)?;
    if r.is_null() {
        return Ok(String::new());
    }
    let s = unsafe { CFString::wrap_under_create_rule(r) };
    Ok(s.to_string())
}

/// Total channels of a device in one scope (from its stream configuration).
pub fn channel_count(id: AudioObjectID, scope: u32) -> u32 {
    let addr = AudioObjectPropertyAddress::new(kAudioDevicePropertyStreamConfiguration, scope, kAudioObjectPropertyElementMain);
    let Ok(buf) = get_bytes(id, addr) else { return 0 };
    let abl = buf.as_ptr() as *const AudioBufferList;
    // The returned size bounds the list; trust mNumberBuffers only within it.
    let max = (buf.len() * 8).saturating_sub(8) / size_of::<AudioBuffer>();
    unsafe {
        let n = ((*abl).mNumberBuffers as usize).min(max);
        let first = std::ptr::addr_of!((*abl).mBuffers) as *const AudioBuffer;
        (0..n).map(|i| (*first.add(i)).mNumberChannels).sum()
    }
}
