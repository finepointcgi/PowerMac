//! The AudioServerPlugIn ABI (CoreAudio/AudioServerPlugIn.h,
//! AudioHardwareBase.h, CoreAudioBaseTypes.h), bound by hand: only what the
//! driver and its harness use.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use crate::spec::fourcc;
use core_foundation_sys::base::CFAllocatorRef;
use core_foundation_sys::string::CFStringRef;
use core_foundation_sys::uuid::{CFUUIDBytes, CFUUIDRef};
use std::ffi::c_void;

pub type OSStatus = i32;
pub type HRESULT = i32;
pub type ULONG = u32;
pub type AudioObjectID = u32;
pub type AudioClassID = u32;
pub type Boolean = u8;
pub type CFPropertyListRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
#[allow(non_camel_case_types)]
pub type pid_t = i32;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioObjectPropertyAddress {
    pub mSelector: u32,
    pub mScope: u32,
    pub mElement: u32,
}

impl AudioObjectPropertyAddress {
    pub const fn new(sel: u32, scope: u32, element: u32) -> Self {
        AudioObjectPropertyAddress { mSelector: sel, mScope: scope, mElement: element }
    }
    pub const fn global(sel: u32) -> Self {
        Self::new(sel, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain)
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
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
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AudioValueRange {
    pub mMinimum: f64,
    pub mMaximum: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AudioStreamRangedDescription {
    pub mFormat: AudioStreamBasicDescription,
    pub mSampleRateRange: AudioValueRange,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SMPTETime {
    pub mSubframes: i16,
    pub mSubframeDivisor: i16,
    pub mCounter: u32,
    pub mType: u32,
    pub mFlags: u32,
    pub mHours: i16,
    pub mMinutes: i16,
    pub mSeconds: i16,
    pub mFrames: i16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioTimeStamp {
    pub mSampleTime: f64,
    pub mHostTime: u64,
    pub mRateScalar: f64,
    pub mWordClockTime: u64,
    pub mSMPTETime: SMPTETime,
    pub mFlags: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioChannelDescription {
    pub mChannelLabel: u32,
    pub mChannelFlags: u32,
    pub mCoordinates: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioServerPlugInCustomPropertyInfo {
    pub mSelector: u32,
    pub mPropertyDataType: u32,
    pub mQualifierDataType: u32,
}

#[repr(C)]
pub struct AudioServerPlugInClientInfo {
    pub mClientID: u32,
    pub mProcessID: pid_t,
    pub mIsNativeEndian: Boolean,
    pub mBundleID: CFStringRef,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioServerPlugInIOCycleInfo {
    pub mIOCycleCounter: u64,
    pub mNominalIOBufferFrameSize: u32,
    pub mCurrentTime: AudioTimeStamp,
    pub mInputTime: AudioTimeStamp,
    pub mOutputTime: AudioTimeStamp,
    pub mMainHostTicksPerFrame: f64,
    pub mDeviceHostTicksPerFrame: f64,
}

pub type AudioServerPlugInHostRef = *const AudioServerPlugInHostInterface;
/// `AudioServerPlugInDriverInterface**`.
pub type AudioServerPlugInDriverRef = *mut c_void;

#[repr(C)]
pub struct AudioServerPlugInHostInterface {
    pub PropertiesChanged: unsafe extern "C" fn(
        AudioServerPlugInHostRef,
        AudioObjectID,
        u32,
        *const AudioObjectPropertyAddress,
    ) -> OSStatus,
    pub CopyFromStorage: unsafe extern "C" fn(AudioServerPlugInHostRef, CFStringRef, *mut CFPropertyListRef) -> OSStatus,
    pub WriteToStorage: unsafe extern "C" fn(AudioServerPlugInHostRef, CFStringRef, CFPropertyListRef) -> OSStatus,
    pub DeleteFromStorage: unsafe extern "C" fn(AudioServerPlugInHostRef, CFStringRef) -> OSStatus,
    pub RequestDeviceConfigurationChange:
        unsafe extern "C" fn(AudioServerPlugInHostRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
}

#[repr(C)]
pub struct AudioServerPlugInDriverInterface {
    pub _reserved: *mut c_void,
    pub QueryInterface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "C" fn(*mut c_void) -> ULONG,
    pub Release: unsafe extern "C" fn(*mut c_void) -> ULONG,
    pub Initialize: unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioServerPlugInHostRef) -> OSStatus,
    pub CreateDevice: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        CFDictionaryRef,
        *const AudioServerPlugInClientInfo,
        *mut AudioObjectID,
    ) -> OSStatus,
    pub DestroyDevice: unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID) -> OSStatus,
    pub AddDeviceClient:
        unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, *const AudioServerPlugInClientInfo) -> OSStatus,
    pub RemoveDeviceClient:
        unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, *const AudioServerPlugInClientInfo) -> OSStatus,
    pub PerformDeviceConfigurationChange:
        unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
    pub AbortDeviceConfigurationChange:
        unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
    pub HasProperty: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        pid_t,
        *const AudioObjectPropertyAddress,
    ) -> Boolean,
    pub IsPropertySettable: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        pid_t,
        *const AudioObjectPropertyAddress,
        *mut Boolean,
    ) -> OSStatus,
    pub GetPropertyDataSize: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        pid_t,
        *const AudioObjectPropertyAddress,
        u32,
        *const c_void,
        *mut u32,
    ) -> OSStatus,
    pub GetPropertyData: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        pid_t,
        *const AudioObjectPropertyAddress,
        u32,
        *const c_void,
        u32,
        *mut u32,
        *mut c_void,
    ) -> OSStatus,
    pub SetPropertyData: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        pid_t,
        *const AudioObjectPropertyAddress,
        u32,
        *const c_void,
        u32,
        *const c_void,
    ) -> OSStatus,
    pub StartIO: unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u32) -> OSStatus,
    pub StopIO: unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u32) -> OSStatus,
    pub GetZeroTimeStamp:
        unsafe extern "C" fn(AudioServerPlugInDriverRef, AudioObjectID, u32, *mut f64, *mut u64, *mut u64) -> OSStatus,
    pub WillDoIOOperation: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        u32,
        u32,
        *mut Boolean,
        *mut Boolean,
    ) -> OSStatus,
    pub BeginIOOperation: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        u32,
        u32,
        u32,
        *const AudioServerPlugInIOCycleInfo,
    ) -> OSStatus,
    pub DoIOOperation: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        AudioObjectID,
        u32,
        u32,
        u32,
        *const AudioServerPlugInIOCycleInfo,
        *mut c_void,
        *mut c_void,
    ) -> OSStatus,
    pub EndIOOperation: unsafe extern "C" fn(
        AudioServerPlugInDriverRef,
        AudioObjectID,
        u32,
        u32,
        u32,
        *const AudioServerPlugInIOCycleInfo,
    ) -> OSStatus,
}

// SAFETY: the vtable is immutable function pointers.
unsafe impl Sync for AudioServerPlugInDriverInterface {}

pub const kAudioHardwareNoError: OSStatus = 0;
pub const kAudioHardwareNotRunningError: OSStatus = fourcc(b"stop") as i32;
pub const kAudioHardwareUnspecifiedError: OSStatus = fourcc(b"what") as i32;
pub const kAudioHardwareUnknownPropertyError: OSStatus = fourcc(b"who?") as i32;
pub const kAudioHardwareBadPropertySizeError: OSStatus = fourcc(b"!siz") as i32;
pub const kAudioHardwareIllegalOperationError: OSStatus = fourcc(b"nope") as i32;
pub const kAudioHardwareBadObjectError: OSStatus = fourcc(b"!obj") as i32;
pub const kAudioHardwareBadDeviceError: OSStatus = fourcc(b"!dev") as i32;
pub const kAudioHardwareBadStreamError: OSStatus = fourcc(b"!str") as i32;
pub const kAudioHardwareUnsupportedOperationError: OSStatus = fourcc(b"unop") as i32;
pub const kAudioDeviceUnsupportedFormatError: OSStatus = fourcc(b"!dat") as i32;

pub const S_OK: HRESULT = 0;
pub const E_NOINTERFACE: HRESULT = 0x8000_0004u32 as i32;

pub const kAudioObjectUnknown: AudioObjectID = 0;
pub const kAudioObjectPlugInObject: AudioObjectID = 1;

pub const kAudioObjectPropertyScopeGlobal: u32 = fourcc(b"glob");
pub const kAudioObjectPropertyScopeInput: u32 = fourcc(b"inpt");
pub const kAudioObjectPropertyScopeOutput: u32 = fourcc(b"outp");
pub const kAudioObjectPropertyScopePlayThrough: u32 = fourcc(b"ptru");
pub const kAudioObjectPropertyElementMain: u32 = 0;

// Classes
pub const kAudioObjectClassID: AudioClassID = fourcc(b"aobj");
pub const kAudioPlugInClassID: AudioClassID = fourcc(b"aplg");
pub const kAudioDeviceClassID: AudioClassID = fourcc(b"adev");
pub const kAudioStreamClassID: AudioClassID = fourcc(b"astr");
pub const kAudioControlClassID: AudioClassID = fourcc(b"actl");
pub const kAudioLevelControlClassID: AudioClassID = fourcc(b"levl");
pub const kAudioVolumeControlClassID: AudioClassID = fourcc(b"vlme");
pub const kAudioBooleanControlClassID: AudioClassID = fourcc(b"togl");
pub const kAudioMuteControlClassID: AudioClassID = fourcc(b"mute");

// AudioObject
pub const kAudioObjectPropertyBaseClass: u32 = fourcc(b"bcls");
pub const kAudioObjectPropertyClass: u32 = fourcc(b"clas");
pub const kAudioObjectPropertyOwner: u32 = fourcc(b"stdv");
pub const kAudioObjectPropertyName: u32 = fourcc(b"lnam");
pub const kAudioObjectPropertyManufacturer: u32 = fourcc(b"lmak");
pub const kAudioObjectPropertyOwnedObjects: u32 = fourcc(b"ownd");
pub const kAudioObjectPropertyCustomPropertyInfoList: u32 = fourcc(b"cust");

// AudioPlugIn
pub const kAudioPlugInPropertyBundleID: u32 = fourcc(b"piid");
pub const kAudioPlugInPropertyDeviceList: u32 = fourcc(b"dev#");
pub const kAudioPlugInPropertyTranslateUIDToDevice: u32 = fourcc(b"uidd");
pub const kAudioPlugInPropertyBoxList: u32 = fourcc(b"box#");
pub const kAudioPlugInPropertyClockDeviceList: u32 = fourcc(b"clk#");
pub const kAudioPlugInPropertyResourceBundle: u32 = fourcc(b"rsrc");

// AudioDevice
pub const kAudioDevicePropertyDeviceUID: u32 = fourcc(b"uid ");
pub const kAudioDevicePropertyModelUID: u32 = fourcc(b"muid");
pub const kAudioDevicePropertyTransportType: u32 = fourcc(b"tran");
pub const kAudioDevicePropertyRelatedDevices: u32 = fourcc(b"akin");
pub const kAudioDevicePropertyClockDomain: u32 = fourcc(b"clkd");
pub const kAudioDevicePropertyDeviceIsAlive: u32 = fourcc(b"livn");
pub const kAudioDevicePropertyDeviceIsRunning: u32 = fourcc(b"goin");
pub const kAudioDevicePropertyDeviceCanBeDefaultDevice: u32 = fourcc(b"dflt");
pub const kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: u32 = fourcc(b"sflt");
pub const kAudioDevicePropertyLatency: u32 = fourcc(b"ltnc");
pub const kAudioDevicePropertyStreams: u32 = fourcc(b"stm#");
pub const kAudioObjectPropertyControlList: u32 = fourcc(b"ctrl");
pub const kAudioDevicePropertySafetyOffset: u32 = fourcc(b"saft");
pub const kAudioDevicePropertyNominalSampleRate: u32 = fourcc(b"nsrt");
pub const kAudioDevicePropertyAvailableNominalSampleRates: u32 = fourcc(b"nsr#");
pub const kAudioDevicePropertyIsHidden: u32 = fourcc(b"hidn");
pub const kAudioDevicePropertyPreferredChannelsForStereo: u32 = fourcc(b"dch2");
pub const kAudioDevicePropertyPreferredChannelLayout: u32 = fourcc(b"srnd");
pub const kAudioDevicePropertyZeroTimeStampPeriod: u32 = fourcc(b"ring");
pub const kAudioDevicePropertyClockIsStable: u32 = fourcc(b"cstb");
pub const kAudioDeviceTransportTypeVirtual: u32 = fourcc(b"virt");

// AudioStream
pub const kAudioStreamPropertyIsActive: u32 = fourcc(b"sact");
pub const kAudioStreamPropertyDirection: u32 = fourcc(b"sdir");
pub const kAudioStreamPropertyTerminalType: u32 = fourcc(b"term");
pub const kAudioStreamPropertyStartingChannel: u32 = fourcc(b"schn");
pub const kAudioStreamPropertyLatency: u32 = fourcc(b"ltnc");
pub const kAudioStreamPropertyVirtualFormat: u32 = fourcc(b"sfmt");
pub const kAudioStreamPropertyAvailableVirtualFormats: u32 = fourcc(b"sfma");
pub const kAudioStreamPropertyPhysicalFormat: u32 = fourcc(b"pft ");
pub const kAudioStreamPropertyAvailablePhysicalFormats: u32 = fourcc(b"pfta");
pub const kAudioStreamTerminalTypeLine: u32 = fourcc(b"line");

// Controls
pub const kAudioControlPropertyScope: u32 = fourcc(b"cscp");
pub const kAudioControlPropertyElement: u32 = fourcc(b"celm");
pub const kAudioLevelControlPropertyScalarValue: u32 = fourcc(b"lcsv");
pub const kAudioLevelControlPropertyDecibelValue: u32 = fourcc(b"lcdv");
pub const kAudioLevelControlPropertyDecibelRange: u32 = fourcc(b"lcdr");
pub const kAudioLevelControlPropertyConvertScalarToDecibels: u32 = fourcc(b"lcsd");
pub const kAudioLevelControlPropertyConvertDecibelsToScalar: u32 = fourcc(b"lcds");
pub const kAudioBooleanControlPropertyValue: u32 = fourcc(b"bcvl");

// Custom property data types
pub const kAudioServerPlugInCustomPropertyDataTypeNone: u32 = 0;
pub const kAudioServerPlugInCustomPropertyDataTypeCFString: u32 = fourcc(b"cfst");

// IO operations
pub const kAudioServerPlugInIOOperationReadInput: u32 = fourcc(b"read");
pub const kAudioServerPlugInIOOperationWriteMix: u32 = fourcc(b"rite");

// Formats and channel layouts
pub const kAudioFormatLinearPCM: u32 = fourcc(b"lpcm");
pub const kAudioFormatFlagIsFloat: u32 = 1 << 0;
pub const kAudioFormatFlagIsBigEndian: u32 = 1 << 1;
pub const kAudioFormatFlagIsPacked: u32 = 1 << 3;
pub const kAudioFormatFlagsNativeFloatPacked: u32 =
    kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked | if cfg!(target_endian = "big") { kAudioFormatFlagIsBigEndian } else { 0 };
pub const kAudioChannelLayoutTag_UseChannelDescriptions: u32 = 0;
pub const kAudioChannelLabel_Left: u32 = 1;
pub const kAudioChannelLabel_Right: u32 = 2;
pub const kAudioChannelLabel_Discrete_0: u32 = 1 << 16;

/// `IUnknownUUID`.
pub const IUNKNOWN_UUID_BYTES: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// `kAudioServerPlugInTypeUUID`.
pub const PLUGIN_TYPE_UUID_BYTES: [u8; 16] =
    [0x44, 0x3A, 0xBA, 0xB8, 0xE7, 0xB3, 0x49, 0x1A, 0xB9, 0x85, 0xBE, 0xB9, 0x18, 0x70, 0x30, 0xDB];
/// `kAudioServerPlugInDriverInterfaceUUID`.
pub const DRIVER_INTERFACE_UUID_BYTES: [u8; 16] =
    [0xEE, 0xA5, 0x77, 0x3D, 0xCC, 0x43, 0x49, 0xF1, 0x8E, 0x00, 0x8F, 0x96, 0xE7, 0xD2, 0x3B, 0x17];

pub fn uuid_bytes(b: &CFUUIDBytes) -> [u8; 16] {
    [
        b.byte0, b.byte1, b.byte2, b.byte3, b.byte4, b.byte5, b.byte6, b.byte7, b.byte8, b.byte9, b.byte10, b.byte11,
        b.byte12, b.byte13, b.byte14, b.byte15,
    ]
}

pub fn to_uuid_bytes(b: [u8; 16]) -> CFUUIDBytes {
    CFUUIDBytes {
        byte0: b[0],
        byte1: b[1],
        byte2: b[2],
        byte3: b[3],
        byte4: b[4],
        byte5: b[5],
        byte6: b[6],
        byte7: b[7],
        byte8: b[8],
        byte9: b[9],
        byte10: b[10],
        byte11: b[11],
        byte12: b[12],
        byte13: b[13],
        byte14: b[14],
        byte15: b[15],
    }
}

#[repr(C)]
pub struct MachTimebaseInfo {
    pub numer: u32,
    pub denom: u32,
}

extern "C" {
    pub fn mach_absolute_time() -> u64;
    pub fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;
}

pub type FactoryFn = unsafe extern "C" fn(CFAllocatorRef, CFUUIDRef) -> *mut c_void;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    /// Sizes from the C headers (LP64).
    #[test]
    fn abi_sizes_match_the_headers() {
        assert_eq!(size_of::<AudioObjectPropertyAddress>(), 12);
        assert_eq!(size_of::<AudioStreamBasicDescription>(), 40);
        assert_eq!(size_of::<AudioValueRange>(), 16);
        assert_eq!(size_of::<AudioStreamRangedDescription>(), 56);
        assert_eq!(size_of::<SMPTETime>(), 24);
        assert_eq!(size_of::<AudioTimeStamp>(), 64);
        assert_eq!(size_of::<AudioChannelDescription>(), 20);
        assert_eq!(size_of::<AudioServerPlugInCustomPropertyInfo>(), 12);
        assert_eq!(size_of::<AudioServerPlugInClientInfo>(), 24);
        assert_eq!(size_of::<AudioServerPlugInIOCycleInfo>(), 8 + 8 + 64 * 3 + 16);
        assert_eq!(size_of::<AudioServerPlugInHostInterface>(), 5 * 8);
        assert_eq!(size_of::<AudioServerPlugInDriverInterface>(), 23 * 8);
        assert_eq!(align_of::<AudioTimeStamp>(), 8);
    }

    #[test]
    fn uuids_match_the_spec_strings() {
        let hex = |b: [u8; 16]| {
            let s: String = b.iter().map(|x| format!("{x:02X}")).collect();
            format!("{}-{}-{}-{}-{}", &s[0..8], &s[8..12], &s[12..16], &s[16..20], &s[20..32])
        };
        assert_eq!(hex(PLUGIN_TYPE_UUID_BYTES), crate::spec::PLUGIN_TYPE_UUID);
        assert_eq!(hex(DRIVER_INTERFACE_UUID_BYTES), crate::spec::DRIVER_INTERFACE_UUID);
    }
}
