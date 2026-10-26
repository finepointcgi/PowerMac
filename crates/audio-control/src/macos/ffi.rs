//! Hand-written CoreAudio bindings (only what we use).
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::c_void;

pub type OSStatus = i32;
pub type AudioObjectID = u32;
pub type AudioDeviceIOProcID = *mut c_void;

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

pub const kAudioObjectSystemObject: AudioObjectID = 1;
pub const kAudioObjectUnknown: AudioObjectID = 0;

pub const kAudioObjectPropertyScopeGlobal: u32 = fourcc(b"glob");
pub const kAudioObjectPropertyScopeInput: u32 = fourcc(b"inpt");
pub const kAudioObjectPropertyScopeOutput: u32 = fourcc(b"outp");
pub const kAudioObjectPropertyElementMain: u32 = 0;

pub const kAudioObjectPropertyName: u32 = fourcc(b"lnam");
pub const kAudioHardwarePropertyDevices: u32 = fourcc(b"dev#");
pub const kAudioHardwarePropertyDefaultInputDevice: u32 = fourcc(b"dIn ");
pub const kAudioHardwarePropertyDefaultOutputDevice: u32 = fourcc(b"dOut");
pub const kAudioHardwarePropertyDefaultSystemOutputDevice: u32 = fourcc(b"sOut");
pub const kAudioHardwarePropertyRunLoop: u32 = fourcc(b"rnlp");
pub const kAudioHardwarePropertyProcessObjectList: u32 = fourcc(b"prs#");
pub const kAudioHardwarePropertyTranslatePIDToProcessObject: u32 = fourcc(b"id2p");
pub const kAudioHardwarePropertyTranslateUIDToDevice: u32 = fourcc(b"uidd");

pub const kAudioDevicePropertyDeviceUID: u32 = fourcc(b"uid ");
pub const kAudioDevicePropertyTransportType: u32 = fourcc(b"tran");
pub const kAudioDevicePropertyStreamConfiguration: u32 = fourcc(b"slay");
pub const kAudioDevicePropertyStreams: u32 = fourcc(b"stm#");
pub const kAudioDevicePropertyVolumeScalar: u32 = fourcc(b"volm");
pub const kAudioDevicePropertyMute: u32 = fourcc(b"mute");
pub const kAudioDevicePropertyPreferredChannelsForStereo: u32 = fourcc(b"dch2");
pub const kAudioDevicePropertyDeviceIsAlive: u32 = fourcc(b"livn");
pub const kAudioDevicePropertyDeviceCanBeDefaultDevice: u32 = fourcc(b"dflt");
pub const kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: u32 = fourcc(b"sflt");
pub const kAudioDevicePropertyIsHidden: u32 = fourcc(b"hidn");
pub const kAudioDevicePropertyNominalSampleRate: u32 = fourcc(b"nsrt");
pub const kAudioDevicePropertyBufferFrameSize: u32 = fourcc(b"fsiz");
pub const kAudioStreamPropertyVirtualFormat: u32 = fourcc(b"sfmt");

pub const kAudioProcessPropertyPID: u32 = fourcc(b"ppid");
pub const kAudioProcessPropertyBundleID: u32 = fourcc(b"pbid");
pub const kAudioProcessPropertyIsRunning: u32 = fourcc(b"pir?");
pub const kAudioProcessPropertyIsRunningOutput: u32 = fourcc(b"piro");

pub const kAudioTapPropertyUID: u32 = fourcc(b"tuid");
pub const kAudioTapPropertyFormat: u32 = fourcc(b"tfmt");

pub const kAudioDeviceTransportTypeBuiltIn: u32 = fourcc(b"bltn");
pub const kAudioDeviceTransportTypeAggregate: u32 = fourcc(b"grup");
pub const kAudioDeviceTransportTypeVirtual: u32 = fourcc(b"virt");
pub const kAudioDeviceTransportTypePCI: u32 = fourcc(b"pci ");
pub const kAudioDeviceTransportTypeUSB: u32 = fourcc(b"usb ");
pub const kAudioDeviceTransportTypeFireWire: u32 = fourcc(b"1394");
pub const kAudioDeviceTransportTypeBluetooth: u32 = fourcc(b"blue");
pub const kAudioDeviceTransportTypeBluetoothLE: u32 = fourcc(b"blea");
pub const kAudioDeviceTransportTypeHDMI: u32 = fourcc(b"hdmi");
pub const kAudioDeviceTransportTypeDisplayPort: u32 = fourcc(b"dprt");
pub const kAudioDeviceTransportTypeAirPlay: u32 = fourcc(b"airp");
pub const kAudioDeviceTransportTypeAVB: u32 = fourcc(b"eavb");
pub const kAudioDeviceTransportTypeThunderbolt: u32 = fourcc(b"thun");
pub const kAudioDeviceTransportTypeContinuityCaptureWired: u32 = fourcc(b"ccwd");
pub const kAudioDeviceTransportTypeContinuityCaptureWireless: u32 = fourcc(b"ccwl");

pub const kAudioFormatLinearPCM: u32 = fourcc(b"lpcm");
pub const kAudioFormatFlagIsFloat: u32 = 1 << 0;
pub const kAudioFormatFlagIsNonInterleaved: u32 = 1 << 5;

pub const kAudioHardwareUnknownPropertyError: OSStatus = fourcc(b"who?") as OSStatus;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioObjectPropertyAddress {
    pub mSelector: u32,
    pub mScope: u32,
    pub mElement: u32,
}

impl AudioObjectPropertyAddress {
    pub const fn new(selector: u32, scope: u32, element: u32) -> Self {
        AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: element }
    }
    pub const fn global(selector: u32) -> Self {
        Self::new(selector, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioBuffer {
    pub mNumberChannels: u32,
    pub mDataByteSize: u32,
    pub mData: *mut c_void,
}

/// Variable-length: `mBuffers` really has `mNumberBuffers` entries.
#[repr(C)]
pub struct AudioBufferList {
    pub mNumberBuffers: u32,
    pub mBuffers: [AudioBuffer; 1],
}

impl AudioBufferList {
    /// # Safety
    /// `this` must point at a valid buffer list.
    pub unsafe fn buffers<'a>(this: *const AudioBufferList) -> &'a [AudioBuffer] {
        if this.is_null() {
            return &[];
        }
        let n = (*this).mNumberBuffers as usize;
        std::slice::from_raw_parts(std::ptr::addr_of!((*this).mBuffers) as *const AudioBuffer, n)
    }

    /// # Safety
    /// `this` must point at a valid buffer list.
    pub unsafe fn buffers_mut<'a>(this: *mut AudioBufferList) -> &'a mut [AudioBuffer] {
        if this.is_null() {
            return &mut [];
        }
        let n = (*this).mNumberBuffers as usize;
        std::slice::from_raw_parts_mut(std::ptr::addr_of_mut!((*this).mBuffers) as *mut AudioBuffer, n)
    }
}

/// Opaque for us (we never read it).
#[repr(C)]
pub struct AudioTimeStamp {
    _private: [u8; 64],
}

pub type AudioObjectPropertyListenerProc = unsafe extern "C" fn(
    in_object_id: AudioObjectID,
    in_number_addresses: u32,
    in_addresses: *const AudioObjectPropertyAddress,
    in_client_data: *mut c_void,
) -> OSStatus;

pub type AudioDeviceIOProc = unsafe extern "C" fn(
    in_device: AudioObjectID,
    in_now: *const AudioTimeStamp,
    in_input_data: *const AudioBufferList,
    in_input_time: *const AudioTimeStamp,
    out_output_data: *mut AudioBufferList,
    in_output_time: *const AudioTimeStamp,
    in_client_data: *mut c_void,
) -> OSStatus;

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    pub fn AudioObjectHasProperty(id: AudioObjectID, addr: *const AudioObjectPropertyAddress) -> u8;
    pub fn AudioObjectIsPropertySettable(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        out_settable: *mut u8,
    ) -> OSStatus;
    pub fn AudioObjectGetPropertyDataSize(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        out_size: *mut u32,
    ) -> OSStatus;
    pub fn AudioObjectGetPropertyData(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        io_size: *mut u32,
        out_data: *mut c_void,
    ) -> OSStatus;
    pub fn AudioObjectSetPropertyData(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> OSStatus;
    pub fn AudioObjectAddPropertyListener(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        listener: AudioObjectPropertyListenerProc,
        client_data: *mut c_void,
    ) -> OSStatus;
    pub fn AudioObjectRemovePropertyListener(
        id: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        listener: AudioObjectPropertyListenerProc,
        client_data: *mut c_void,
    ) -> OSStatus;

    pub fn AudioHardwareCreateAggregateDevice(
        description: core_foundation_sys::dictionary::CFDictionaryRef,
        out_device: *mut AudioObjectID,
    ) -> OSStatus;
    pub fn AudioHardwareDestroyAggregateDevice(device: AudioObjectID) -> OSStatus;

    pub fn AudioDeviceCreateIOProcID(
        device: AudioObjectID,
        proc_: AudioDeviceIOProc,
        client_data: *mut c_void,
        out_proc_id: *mut AudioDeviceIOProcID,
    ) -> OSStatus;
    pub fn AudioDeviceDestroyIOProcID(device: AudioObjectID, proc_id: AudioDeviceIOProcID) -> OSStatus;
    pub fn AudioDeviceStart(device: AudioObjectID, proc_id: AudioDeviceIOProcID) -> OSStatus;
    pub fn AudioDeviceStop(device: AudioObjectID, proc_id: AudioDeviceIOProcID) -> OSStatus;
}

extern "C" {
    pub fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
}

/// `OSStatus` → readable error (four-char codes are printed as text).
pub fn status_str(st: OSStatus) -> String {
    let b = (st as u32).to_be_bytes();
    if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
        format!("'{}' ({st})", String::from_utf8_lossy(&b))
    } else {
        st.to_string()
    }
}

pub fn check(st: OSStatus, what: &str) -> anyhow::Result<()> {
    if st == 0 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{what} failed: OSStatus {}", status_str(st)))
    }
}
