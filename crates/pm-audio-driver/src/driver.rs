//! The AudioServerPlugIn: object model, property tables, configuration and
//! IO.
//!
//! ## Objects
//!
//! | id | object |
//! |---|---|
//! | 1 | the plug-in (`kAudioObjectPlugInObject`) |
//! | 16 + 8·slot | device |
//! | +1 / +2 | input stream / output stream |
//! | +3 / +4 | output volume / output mute control |
//!
//! A device keeps its slot (and so its object ids) for as long as its uid
//! stays in the configuration.
//!
//! ## Threads and locks
//!
//! `state` (configuration, names, stream activity, pending format changes)
//! is taken by property calls on the HAL's worker threads. Each slot has its
//! own `io` lock (ring buffer, clock, running count) taken by the IO calls on
//! the device's real-time thread and, briefly, by Start/StopIO and
//! configuration changes. Lock order: `state` → `io`. Host callbacks are
//! never made with a lock held: mutating calls return [`HostAction`]s that
//! the entry points run after unlocking.
//!
//! The ring buffer is only reallocated in `PerformDeviceConfigurationChange`,
//! when the host guarantees IO is stopped; `read`/`write` never allocate.

#![allow(non_upper_case_globals)]

use crate::clock::{host_ticks_per_second, Clock};
use crate::ring::LoopbackRing;
use crate::spec::*;
use crate::sys::*;
use crate::volume;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFAllocatorRef, CFGetTypeID, CFRelease};
use core_foundation_sys::string::{CFStringGetTypeID, CFStringRef};
use core_foundation_sys::uuid::{CFUUIDBytes, CFUUIDRef};
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

pub const FIRST_DEVICE_ID: u32 = 16;
pub const OBJECTS_PER_DEVICE: u32 = 8;
const MODEL_UID: &str = "PowerMacAudio:model";

/// What an object id refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Obj {
    PlugIn,
    Device(usize),
    InputStream(usize),
    OutputStream(usize),
    Volume(usize),
    Mute(usize),
}

impl Obj {
    pub fn id(self) -> AudioObjectID {
        let base = |s: usize| FIRST_DEVICE_ID + s as u32 * OBJECTS_PER_DEVICE;
        match self {
            Obj::PlugIn => kAudioObjectPlugInObject,
            Obj::Device(s) => base(s),
            Obj::InputStream(s) => base(s) + 1,
            Obj::OutputStream(s) => base(s) + 2,
            Obj::Volume(s) => base(s) + 3,
            Obj::Mute(s) => base(s) + 4,
        }
    }

    pub fn from_id(id: AudioObjectID) -> Option<Obj> {
        if id == kAudioObjectPlugInObject {
            return Some(Obj::PlugIn);
        }
        let rel = id.checked_sub(FIRST_DEVICE_ID)?;
        let slot = (rel / OBJECTS_PER_DEVICE) as usize;
        if slot >= MAX_DEVICES {
            return None;
        }
        Some(match rel % OBJECTS_PER_DEVICE {
            0 => Obj::Device(slot),
            1 => Obj::InputStream(slot),
            2 => Obj::OutputStream(slot),
            3 => Obj::Volume(slot),
            4 => Obj::Mute(slot),
            _ => return None,
        })
    }

    fn slot(self) -> Option<usize> {
        match self {
            Obj::PlugIn => None,
            Obj::Device(s) | Obj::InputStream(s) | Obj::OutputStream(s) | Obj::Volume(s) | Obj::Mute(s) => Some(s),
        }
    }

    /// (class, base class).
    fn classes(self) -> (AudioClassID, AudioClassID) {
        match self {
            Obj::PlugIn => (kAudioPlugInClassID, kAudioObjectClassID),
            Obj::Device(_) => (kAudioDeviceClassID, kAudioObjectClassID),
            Obj::InputStream(_) | Obj::OutputStream(_) => (kAudioStreamClassID, kAudioObjectClassID),
            Obj::Volume(_) => (kAudioVolumeControlClassID, kAudioLevelControlClassID),
            Obj::Mute(_) => (kAudioMuteControlClassID, kAudioBooleanControlClassID),
        }
    }

    fn is_a(self, class: AudioClassID) -> bool {
        let (c, base) = self.classes();
        class == kAudioObjectClassID
            || class == c
            || class == base
            || (class == kAudioControlClassID && matches!(self, Obj::Volume(_) | Obj::Mute(_)))
    }
}

/// A host callback to make once no lock is held.
#[derive(Debug, Clone, PartialEq)]
pub enum HostAction {
    Changed(AudioObjectID, Vec<AudioObjectPropertyAddress>),
    RequestConfigChange(AudioObjectID),
    Store(String),
}

struct DeviceState {
    spec: DeviceSpec,
    /// Format (channels / rate) waiting for PerformDeviceConfigurationChange.
    pending: Option<DeviceSpec>,
    input_active: bool,
    output_active: bool,
}

impl DeviceState {
    fn wanted(&self) -> &DeviceSpec {
        self.pending.as_ref().unwrap_or(&self.spec)
    }
}

pub(crate) struct IoState {
    running: u32,
    ring: LoopbackRing,
    clock: Clock,
}

struct Slot {
    present: AtomicBool,
    /// f32 bits of the volume scalar.
    volume: AtomicU32,
    muted: AtomicBool,
    io: Mutex<IoState>,
}

struct State {
    devices: Vec<Option<DeviceState>>,
    /// Slots in configuration order.
    order: Vec<usize>,
    initialized: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A property value, written to the caller's buffer by [`Value::write`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U32(u32),
    F32(f32),
    F64(f64),
    /// u32 array (object ids, class ids); may be truncated to the buffer.
    Ids(Vec<u32>),
    /// Returned as a +1 CFStringRef.
    Str(String),
    /// Raw struct bytes; `elem > 0`: an array of `elem`-byte items that may
    /// be truncated, `elem == 0`: must fit whole.
    Pod(Vec<u8>, usize),
}

fn pod_slice<T: Copy>(items: &[T]) -> Value {
    let bytes = unsafe { std::slice::from_raw_parts(items.as_ptr() as *const u8, std::mem::size_of_val(items)) };
    Value::Pod(bytes.to_vec(), size_of::<T>())
}

fn pod_whole<T: Copy>(item: &T) -> Value {
    let bytes = unsafe { std::slice::from_raw_parts(item as *const T as *const u8, size_of::<T>()) };
    Value::Pod(bytes.to_vec(), 0)
}

impl Value {
    pub fn size(&self) -> u32 {
        (match self {
            Value::U32(_) | Value::F32(_) => 4,
            Value::F64(_) => 8,
            Value::Ids(v) => v.len() * 4,
            Value::Str(_) => size_of::<CFStringRef>(),
            Value::Pod(b, _) => b.len(),
        }) as u32
    }

    /// # Safety
    /// `out` must be valid for `cap` bytes.
    unsafe fn write(&self, out: *mut c_void, cap: u32) -> Result<u32, OSStatus> {
        let cap = cap as usize;
        let need = self.size() as usize;
        let fixed = |n: usize| if cap < n { Err(kAudioHardwareBadPropertySizeError) } else { Ok(()) };
        if out.is_null() && need > 0 {
            return Err(kAudioHardwareIllegalOperationError);
        }
        match self {
            Value::U32(v) => {
                fixed(4)?;
                (out as *mut u32).write_unaligned(*v);
                Ok(4)
            }
            Value::F32(v) => {
                fixed(4)?;
                (out as *mut f32).write_unaligned(*v);
                Ok(4)
            }
            Value::F64(v) => {
                fixed(8)?;
                (out as *mut f64).write_unaligned(*v);
                Ok(8)
            }
            Value::Ids(v) => {
                let n = v.len().min(cap / 4);
                std::ptr::copy_nonoverlapping(v.as_ptr() as *const u8, out as *mut u8, n * 4);
                Ok((n * 4) as u32)
            }
            Value::Str(s) => {
                fixed(size_of::<CFStringRef>())?;
                let cf = CFString::new(s);
                let r = cf.as_concrete_TypeRef();
                std::mem::forget(cf); // the caller owns the +1 reference
                (out as *mut CFStringRef).write_unaligned(r);
                Ok(size_of::<CFStringRef>() as u32)
            }
            Value::Pod(b, elem) => {
                let n = if *elem == 0 {
                    fixed(b.len())?;
                    b.len()
                } else {
                    (cap / elem * elem).min(b.len())
                };
                std::ptr::copy_nonoverlapping(b.as_ptr(), out as *mut u8, n);
                Ok(n as u32)
            }
        }
    }
}

/// Float32, interleaved, packed, native-endian.
pub fn stream_format(rate: f64, channels: u32) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: rate,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: 4 * channels,
        mFramesPerPacket: 1,
        mBytesPerFrame: 4 * channels,
        mChannelsPerFrame: channels,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

fn format_acceptable(f: &AudioStreamBasicDescription, channels: u32) -> bool {
    f.mFormatID == kAudioFormatLinearPCM
        && f.mFormatFlags & (kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked) == (kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked)
        && f.mFormatFlags & kAudioFormatFlagIsBigEndian == kAudioFormatFlagsNativeFloatPacked & kAudioFormatFlagIsBigEndian
        && f.mChannelsPerFrame == channels
        && f.mBitsPerChannel == 32
        && f.mFramesPerPacket == 1
        && f.mBytesPerFrame == 4 * channels
        && is_supported_rate(f.mSampleRate)
}

/// `AudioChannelLayout` with one description per channel: Left/Right for
/// stereo, Discrete_n otherwise.
pub fn channel_layout(channels: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(12 + channels as usize * 20);
    b.extend_from_slice(&kAudioChannelLayoutTag_UseChannelDescriptions.to_ne_bytes());
    b.extend_from_slice(&0u32.to_ne_bytes()); // bitmap
    b.extend_from_slice(&channels.to_ne_bytes());
    for i in 0..channels {
        let label = match (channels, i) {
            (2, 0) => kAudioChannelLabel_Left,
            (2, 1) => kAudioChannelLabel_Right,
            _ => kAudioChannelLabel_Discrete_0 | i,
        };
        b.extend_from_slice(&label.to_ne_bytes());
        b.extend_from_slice(&0u32.to_ne_bytes()); // flags
        b.extend_from_slice(&[0u8; 12]); // coordinates
    }
    b
}

pub struct Driver {
    refs: AtomicU32,
    host: AtomicPtr<AudioServerPlugInHostInterface>,
    ticks_per_second: f64,
    state: Mutex<State>,
    slots: Vec<Slot>,
}

impl Default for Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver {
    pub fn new() -> Self {
        let mut tb = MachTimebaseInfo { numer: 1, denom: 1 };
        unsafe { mach_timebase_info(&mut tb) };
        let tps = host_ticks_per_second(tb.numer, tb.denom);
        let slots = (0..MAX_DEVICES)
            .map(|_| Slot {
                present: AtomicBool::new(false),
                volume: AtomicU32::new(1.0f32.to_bits()),
                muted: AtomicBool::new(false),
                io: Mutex::new(IoState { running: 0, ring: LoopbackRing::new(1, 1), clock: Clock::new(48_000.0, RING_FRAMES, tps) }),
            })
            .collect();
        Driver {
            refs: AtomicU32::new(1),
            host: AtomicPtr::new(std::ptr::null_mut()),
            ticks_per_second: tps,
            state: Mutex::new(State { devices: (0..MAX_DEVICES).map(|_| None).collect(), order: Vec::new(), initialized: false }),
            slots,
        }
    }

    // ---- configuration ------------------------------------------------

    /// The configuration as requested (pending formats included).
    pub fn config(&self) -> DriverConfig {
        let st = lock(&self.state);
        DriverConfig { devices: st.order.iter().filter_map(|&s| st.devices[s].as_ref().map(|d| d.wanted().clone())).collect() }
    }

    /// Device object ids in configuration order.
    pub fn device_ids(&self) -> Vec<AudioObjectID> {
        lock(&self.state).order.iter().map(|&s| Obj::Device(s).id()).collect()
    }

    fn create_device(&self, st: &mut State, slot: usize, spec: &DeviceSpec) {
        {
            let mut io = lock(&self.slots[slot].io);
            io.running = 0;
            io.ring.reset(RING_FRAMES as usize, spec.channels as usize);
            io.clock.set_rate(spec.sample_rate, self.ticks_per_second);
        }
        let s = &self.slots[slot];
        s.volume.store(1.0f32.to_bits(), Ordering::Relaxed);
        s.muted.store(false, Ordering::Relaxed);
        st.devices[slot] = Some(DeviceState { spec: spec.clone(), pending: None, input_active: true, output_active: true });
        s.present.store(true, Ordering::Release);
    }

    /// Make `cfg` (validated) the device set. `initial`: during Initialize
    /// (no notifications, nothing stored).
    pub fn apply_config(&self, cfg: &DriverConfig, initial: bool) -> Vec<HostAction> {
        let mut actions = Vec::new();
        let mut st = lock(&self.state);
        let mut list_changed = false;
        for slot in 0..MAX_DEVICES {
            let gone = st.devices[slot].as_ref().is_some_and(|d| cfg.device(&d.spec.uid).is_none());
            if gone {
                self.slots[slot].present.store(false, Ordering::Release);
                st.devices[slot] = None;
                let mut io = lock(&self.slots[slot].io);
                io.running = 0;
                io.ring.reset(1, 1);
                list_changed = true;
            }
        }
        let mut order = Vec::new();
        for spec in &cfg.devices {
            let existing = (0..MAX_DEVICES).find(|&s| st.devices[s].as_ref().is_some_and(|d| d.spec.uid == spec.uid));
            match existing {
                Some(slot) => {
                    let d = st.devices[slot].as_mut().unwrap();
                    let dev = Obj::Device(slot).id();
                    if d.spec.name != spec.name {
                        d.spec.name = spec.name.clone();
                        actions.push(HostAction::Changed(dev, vec![AudioObjectPropertyAddress::global(kAudioObjectPropertyName)]));
                    }
                    if d.spec.format_differs(spec) {
                        let already = d.pending.as_ref().is_some_and(|p| !p.format_differs(spec));
                        d.pending = Some(spec.clone());
                        if !already {
                            actions.push(HostAction::RequestConfigChange(dev));
                        }
                    } else {
                        d.pending = None;
                    }
                    order.push(slot);
                }
                None => {
                    let Some(slot) = (0..MAX_DEVICES).find(|&s| st.devices[s].is_none() && !order.contains(&s)) else { continue };
                    self.create_device(&mut st, slot, spec);
                    order.push(slot);
                    list_changed = true;
                }
            }
        }
        if st.order != order {
            list_changed = true;
        }
        st.order = order;
        st.initialized = true;
        drop(st);
        if !initial {
            if list_changed {
                actions.push(HostAction::Changed(
                    kAudioObjectPlugInObject,
                    vec![
                        AudioObjectPropertyAddress::global(kAudioPlugInPropertyDeviceList),
                        AudioObjectPropertyAddress::global(kAudioObjectPropertyOwnedObjects),
                    ],
                ));
            }
            actions.push(HostAction::Store(cfg.to_json()));
        }
        actions
    }

    /// Apply a pending format change (the host has stopped IO).
    pub fn perform_config_change(&self, device: AudioObjectID) -> Result<Vec<HostAction>, OSStatus> {
        let Some(Obj::Device(slot)) = Obj::from_id(device) else { return Err(kAudioHardwareBadDeviceError) };
        let mut st = lock(&self.state);
        let Some(d) = st.devices[slot].as_mut() else { return Err(kAudioHardwareBadDeviceError) };
        let Some(p) = d.pending.take() else { return Ok(Vec::new()) };
        let channels_changed = p.channels != d.spec.channels;
        d.spec.channels = p.channels;
        d.spec.sample_rate = p.sample_rate;
        {
            let mut io = lock(&self.slots[slot].io);
            if channels_changed {
                io.ring.reset(RING_FRAMES as usize, p.channels as usize);
            } else {
                io.ring.clear();
            }
            io.clock.set_rate(p.sample_rate, self.ticks_per_second);
        }
        drop(st);
        Ok(vec![HostAction::Store(self.config().to_json())])
    }

    pub fn abort_config_change(&self, device: AudioObjectID) {
        if let Some(Obj::Device(slot)) = Obj::from_id(device) {
            if let Some(d) = lock(&self.state).devices[slot].as_mut() {
                d.pending = None;
            }
        }
    }

    // ---- properties ---------------------------------------------------

    fn device_objects(slot: usize, scope: u32) -> Vec<Obj> {
        match scope {
            kAudioObjectPropertyScopeInput => vec![Obj::InputStream(slot)],
            kAudioObjectPropertyScopeOutput => vec![Obj::OutputStream(slot), Obj::Volume(slot), Obj::Mute(slot)],
            _ => vec![Obj::InputStream(slot), Obj::OutputStream(slot), Obj::Volume(slot), Obj::Mute(slot)],
        }
    }

    fn owned(objs: Vec<Obj>, qualifier: &[u8]) -> Value {
        let classes: Vec<u32> = qualifier.as_chunks::<4>().0.iter().map(|c| u32::from_ne_bytes(*c)).collect();
        Value::Ids(objs.into_iter().filter(|o| classes.is_empty() || classes.iter().any(|&c| o.is_a(c))).map(Obj::id).collect())
    }

    fn ranged_formats(channels: u32) -> Value {
        let v: Vec<AudioStreamRangedDescription> = SAMPLE_RATES
            .iter()
            .map(|&r| AudioStreamRangedDescription { mFormat: stream_format(r, channels), mSampleRateRange: AudioValueRange { mMinimum: r, mMaximum: r } })
            .collect();
        pod_slice(&v)
    }

    /// A property's value. `qualifier`: the qualifier bytes; `input`: the
    /// caller's buffer contents (used by the dB/scalar translations).
    pub fn get(&self, id: AudioObjectID, addr: &AudioObjectPropertyAddress, qualifier: &[u8], input: Option<f32>) -> Result<Value, OSStatus> {
        let obj = Obj::from_id(id).ok_or(kAudioHardwareBadObjectError)?;
        let sel = addr.mSelector;
        let st = lock(&self.state);
        if obj == Obj::PlugIn {
            return Ok(match sel {
                kAudioObjectPropertyBaseClass => Value::U32(kAudioObjectClassID),
                kAudioObjectPropertyClass => Value::U32(kAudioPlugInClassID),
                kAudioObjectPropertyOwner => Value::U32(kAudioObjectUnknown),
                kAudioObjectPropertyManufacturer => Value::Str(MANUFACTURER.into()),
                kAudioPlugInPropertyBundleID => Value::Str(BUNDLE_ID.into()),
                kAudioObjectPropertyOwnedObjects => {
                    Self::owned(st.order.iter().map(|&s| Obj::Device(s)).collect(), qualifier)
                }
                kAudioPlugInPropertyDeviceList => Value::Ids(st.order.iter().map(|&s| Obj::Device(s).id()).collect()),
                kAudioPlugInPropertyTranslateUIDToDevice => {
                    let uid = if qualifier.len() >= size_of::<CFStringRef>() {
                        let r = unsafe { (qualifier.as_ptr() as *const CFStringRef).read_unaligned() };
                        cfstring_to_string(r)
                    } else {
                        None
                    };
                    let found = uid.and_then(|u| {
                        st.order.iter().copied().find(|&s| st.devices[s].as_ref().is_some_and(|d| d.spec.uid == u))
                    });
                    Value::U32(found.map_or(kAudioObjectUnknown, |s| Obj::Device(s).id()))
                }
                kAudioPlugInPropertyResourceBundle => Value::Str(String::new()),
                kAudioPlugInPropertyBoxList | kAudioPlugInPropertyClockDeviceList => Value::Ids(Vec::new()),
                kAudioObjectPropertyCustomPropertyInfoList => pod_slice(&[AudioServerPlugInCustomPropertyInfo {
                    mSelector: CONFIG_PROPERTY,
                    mPropertyDataType: kAudioServerPlugInCustomPropertyDataTypeCFString,
                    mQualifierDataType: kAudioServerPlugInCustomPropertyDataTypeNone,
                }]),
                CONFIG_PROPERTY => {
                    let cfg = DriverConfig {
                        devices: st.order.iter().filter_map(|&s| st.devices[s].as_ref().map(|d| d.wanted().clone())).collect(),
                    };
                    Value::Str(cfg.to_json())
                }
                _ => return Err(kAudioHardwareUnknownPropertyError),
            });
        }
        let slot = obj.slot().unwrap();
        let d = st.devices[slot].as_ref().ok_or(kAudioHardwareBadObjectError)?;
        let spec = &d.spec;
        let dev_id = Obj::Device(slot).id();
        let common = |class: u32, base: u32| -> Option<Value> {
            Some(match sel {
                kAudioObjectPropertyBaseClass => Value::U32(base),
                kAudioObjectPropertyClass => Value::U32(class),
                kAudioObjectPropertyOwner => Value::U32(if class == kAudioDeviceClassID { kAudioObjectPlugInObject } else { dev_id }),
                _ => return None,
            })
        };
        let (class, base) = obj.classes();
        if let Some(v) = common(class, base) {
            return Ok(v);
        }
        match obj {
            Obj::Device(_) => Ok(match sel {
                kAudioObjectPropertyName => Value::Str(spec.name.clone()),
                kAudioObjectPropertyManufacturer => Value::Str(MANUFACTURER.into()),
                kAudioObjectPropertyOwnedObjects => Self::owned(Self::device_objects(slot, addr.mScope), qualifier),
                kAudioDevicePropertyDeviceUID => Value::Str(spec.uid.clone()),
                kAudioDevicePropertyModelUID => Value::Str(MODEL_UID.into()),
                kAudioDevicePropertyTransportType => Value::U32(kAudioDeviceTransportTypeVirtual),
                kAudioDevicePropertyRelatedDevices => Value::Ids(vec![dev_id]),
                kAudioDevicePropertyClockDomain => Value::U32(0),
                kAudioDevicePropertyDeviceIsAlive => Value::U32(1),
                kAudioDevicePropertyDeviceIsRunning => Value::U32((lock(&self.slots[slot].io).running > 0) as u32),
                kAudioDevicePropertyDeviceCanBeDefaultDevice | kAudioDevicePropertyDeviceCanBeDefaultSystemDevice => Value::U32(1),
                kAudioDevicePropertyLatency | kAudioDevicePropertySafetyOffset => Value::U32(0),
                kAudioDevicePropertyStreams => Value::Ids(
                    Self::device_objects(slot, addr.mScope)
                        .into_iter()
                        .filter(|o| matches!(o, Obj::InputStream(_) | Obj::OutputStream(_)))
                        .map(Obj::id)
                        .collect(),
                ),
                kAudioObjectPropertyControlList => Value::Ids(vec![Obj::Volume(slot).id(), Obj::Mute(slot).id()]),
                kAudioDevicePropertyNominalSampleRate => Value::F64(spec.sample_rate),
                kAudioDevicePropertyAvailableNominalSampleRates => {
                    let v: Vec<AudioValueRange> = SAMPLE_RATES.iter().map(|&r| AudioValueRange { mMinimum: r, mMaximum: r }).collect();
                    pod_slice(&v)
                }
                kAudioDevicePropertyIsHidden => Value::U32(0),
                kAudioDevicePropertyPreferredChannelsForStereo => pod_whole(&[1u32, 2u32]),
                kAudioDevicePropertyPreferredChannelLayout => Value::Pod(channel_layout(spec.channels), 0),
                kAudioDevicePropertyZeroTimeStampPeriod => Value::U32(RING_FRAMES),
                kAudioDevicePropertyClockIsStable => Value::U32(1),
                _ => return Err(kAudioHardwareUnknownPropertyError),
            }),
            Obj::InputStream(_) | Obj::OutputStream(_) => {
                let input = matches!(obj, Obj::InputStream(_));
                Ok(match sel {
                    kAudioObjectPropertyOwnedObjects => Value::Ids(Vec::new()),
                    kAudioStreamPropertyIsActive => Value::U32(if input { d.input_active } else { d.output_active } as u32),
                    kAudioStreamPropertyDirection => Value::U32(input as u32),
                    kAudioStreamPropertyTerminalType => Value::U32(kAudioStreamTerminalTypeLine),
                    kAudioStreamPropertyStartingChannel => Value::U32(1),
                    kAudioStreamPropertyLatency => Value::U32(0),
                    kAudioStreamPropertyVirtualFormat | kAudioStreamPropertyPhysicalFormat => {
                        pod_whole(&stream_format(spec.sample_rate, spec.channels))
                    }
                    kAudioStreamPropertyAvailableVirtualFormats | kAudioStreamPropertyAvailablePhysicalFormats => {
                        Self::ranged_formats(spec.channels)
                    }
                    _ => return Err(kAudioHardwareUnknownPropertyError),
                })
            }
            Obj::Volume(_) => {
                let scalar = f32::from_bits(self.slots[slot].volume.load(Ordering::Relaxed));
                Ok(match sel {
                    kAudioObjectPropertyOwnedObjects => Value::Ids(Vec::new()),
                    kAudioControlPropertyScope => Value::U32(kAudioObjectPropertyScopeOutput),
                    kAudioControlPropertyElement => Value::U32(kAudioObjectPropertyElementMain),
                    kAudioLevelControlPropertyScalarValue => Value::F32(scalar),
                    kAudioLevelControlPropertyDecibelValue => Value::F32(volume::scalar_to_db(scalar)),
                    kAudioLevelControlPropertyDecibelRange => {
                        pod_whole(&AudioValueRange { mMinimum: volume::MIN_DB as f64, mMaximum: volume::MAX_DB as f64 })
                    }
                    kAudioLevelControlPropertyConvertScalarToDecibels => Value::F32(volume::scalar_to_db(input.unwrap_or(0.0))),
                    kAudioLevelControlPropertyConvertDecibelsToScalar => {
                        Value::F32(volume::db_to_scalar(input.unwrap_or(volume::MIN_DB)))
                    }
                    _ => return Err(kAudioHardwareUnknownPropertyError),
                })
            }
            Obj::Mute(_) => Ok(match sel {
                kAudioObjectPropertyOwnedObjects => Value::Ids(Vec::new()),
                kAudioControlPropertyScope => Value::U32(kAudioObjectPropertyScopeOutput),
                kAudioControlPropertyElement => Value::U32(kAudioObjectPropertyElementMain),
                kAudioBooleanControlPropertyValue => Value::U32(self.slots[slot].muted.load(Ordering::Relaxed) as u32),
                _ => return Err(kAudioHardwareUnknownPropertyError),
            }),
            Obj::PlugIn => unreachable!(),
        }
    }

    pub fn has(&self, id: AudioObjectID, addr: &AudioObjectPropertyAddress) -> bool {
        self.get(id, addr, &[], None).is_ok()
    }

    pub fn settable(&self, id: AudioObjectID, addr: &AudioObjectPropertyAddress) -> Result<bool, OSStatus> {
        self.get(id, addr, &[], None)?;
        let obj = Obj::from_id(id).ok_or(kAudioHardwareBadObjectError)?;
        Ok(matches!(
            (obj, addr.mSelector),
            (Obj::PlugIn, CONFIG_PROPERTY)
                | (Obj::Device(_), kAudioDevicePropertyNominalSampleRate)
                | (
                    Obj::InputStream(_) | Obj::OutputStream(_),
                    kAudioStreamPropertyIsActive | kAudioStreamPropertyVirtualFormat | kAudioStreamPropertyPhysicalFormat
                )
                | (Obj::Volume(_), kAudioLevelControlPropertyScalarValue | kAudioLevelControlPropertyDecibelValue)
                | (Obj::Mute(_), kAudioBooleanControlPropertyValue)
        ))
    }

    /// Queue a sample-rate change for a device (the host is asked to stop
    /// IO and call PerformDeviceConfigurationChange).
    fn request_rate(&self, slot: usize, rate: f64) -> Result<Vec<HostAction>, OSStatus> {
        if !is_supported_rate(rate) {
            return Err(kAudioDeviceUnsupportedFormatError);
        }
        let mut st = lock(&self.state);
        let d = st.devices[slot].as_mut().ok_or(kAudioHardwareBadObjectError)?;
        if d.wanted().sample_rate == rate {
            return Ok(Vec::new());
        }
        let mut p = d.wanted().clone();
        p.sample_rate = rate;
        let had_pending = d.pending.is_some();
        d.pending = if p.format_differs(&d.spec) { Some(p) } else { None };
        Ok(if had_pending || d.pending.is_none() { Vec::new() } else { vec![HostAction::RequestConfigChange(Obj::Device(slot).id())] })
    }

    /// Set a property. `data`: the caller's bytes.
    pub fn set(&self, id: AudioObjectID, addr: &AudioObjectPropertyAddress, data: &[u8]) -> Result<Vec<HostAction>, OSStatus> {
        if !self.settable(id, addr)? {
            return Err(kAudioHardwareUnsupportedOperationError);
        }
        let obj = Obj::from_id(id).ok_or(kAudioHardwareBadObjectError)?;
        let u32_at = || -> Result<u32, OSStatus> {
            (data.len() >= 4).then(|| u32::from_ne_bytes([data[0], data[1], data[2], data[3]])).ok_or(kAudioHardwareBadPropertySizeError)
        };
        let f32_at = || u32_at().map(f32::from_bits);
        match (obj, addr.mSelector) {
            (Obj::PlugIn, CONFIG_PROPERTY) => {
                if data.len() < size_of::<CFStringRef>() {
                    return Err(kAudioHardwareBadPropertySizeError);
                }
                let r = unsafe { (data.as_ptr() as *const CFStringRef).read_unaligned() };
                let json = cfstring_to_string(r).ok_or(kAudioHardwareIllegalOperationError)?;
                let cfg = DriverConfig::from_json(&json).map_err(|_| kAudioHardwareIllegalOperationError)?;
                Ok(self.apply_config(&cfg, false))
            }
            (Obj::Device(slot), kAudioDevicePropertyNominalSampleRate) => {
                if data.len() < 8 {
                    return Err(kAudioHardwareBadPropertySizeError);
                }
                let rate = f64::from_ne_bytes(data[..8].try_into().unwrap());
                self.request_rate(slot, rate)
            }
            (Obj::InputStream(slot) | Obj::OutputStream(slot), kAudioStreamPropertyIsActive) => {
                let on = u32_at()? != 0;
                let mut st = lock(&self.state);
                let d = st.devices[slot].as_mut().ok_or(kAudioHardwareBadObjectError)?;
                if matches!(obj, Obj::InputStream(_)) {
                    d.input_active = on;
                } else {
                    d.output_active = on;
                }
                Ok(vec![HostAction::Changed(id, vec![AudioObjectPropertyAddress::global(kAudioStreamPropertyIsActive)])])
            }
            (Obj::InputStream(slot) | Obj::OutputStream(slot), _) => {
                if data.len() < size_of::<AudioStreamBasicDescription>() {
                    return Err(kAudioHardwareBadPropertySizeError);
                }
                let f = unsafe { (data.as_ptr() as *const AudioStreamBasicDescription).read_unaligned() };
                let channels = lock(&self.state).devices[slot].as_ref().ok_or(kAudioHardwareBadObjectError)?.spec.channels;
                if !format_acceptable(&f, channels) {
                    return Err(kAudioDeviceUnsupportedFormatError);
                }
                self.request_rate(slot, f.mSampleRate)
            }
            (Obj::Volume(slot), sel) => {
                let v = f32_at()?;
                let scalar = if sel == kAudioLevelControlPropertyDecibelValue { volume::db_to_scalar(v) } else { v.clamp(0.0, 1.0) };
                let old = self.slots[slot].volume.swap(scalar.to_bits(), Ordering::Relaxed);
                Ok(if old == scalar.to_bits() {
                    Vec::new()
                } else {
                    vec![HostAction::Changed(
                        id,
                        vec![
                            AudioObjectPropertyAddress::global(kAudioLevelControlPropertyScalarValue),
                            AudioObjectPropertyAddress::global(kAudioLevelControlPropertyDecibelValue),
                        ],
                    )]
                })
            }
            (Obj::Mute(slot), _) => {
                let m = u32_at()? != 0;
                let old = self.slots[slot].muted.swap(m, Ordering::Relaxed);
                Ok(if old == m {
                    Vec::new()
                } else {
                    vec![HostAction::Changed(id, vec![AudioObjectPropertyAddress::global(kAudioBooleanControlPropertyValue)])]
                })
            }
            _ => Err(kAudioHardwareUnsupportedOperationError),
        }
    }

    // ---- IO -------------------------------------------------------------

    fn device_slot(&self, device: AudioObjectID) -> Result<&Slot, OSStatus> {
        match Obj::from_id(device) {
            Some(Obj::Device(s)) if self.slots[s].present.load(Ordering::Acquire) => Ok(&self.slots[s]),
            _ => Err(kAudioHardwareBadObjectError),
        }
    }

    pub fn start_io(&self, device: AudioObjectID) -> Result<(), OSStatus> {
        let slot = self.device_slot(device)?;
        let mut io = lock(&slot.io);
        if io.running == 0 {
            io.ring.clear();
            io.clock.start(unsafe { mach_absolute_time() });
        }
        io.running += 1;
        Ok(())
    }

    pub fn stop_io(&self, device: AudioObjectID) -> Result<(), OSStatus> {
        let slot = self.device_slot(device)?;
        let mut io = lock(&slot.io);
        if io.running == 0 {
            return Err(kAudioHardwareNotRunningError);
        }
        io.running -= 1;
        Ok(())
    }

    pub fn zero_timestamp(&self, device: AudioObjectID) -> Result<(f64, u64, u64), OSStatus> {
        let slot = self.device_slot(device)?;
        let mut io = lock(&slot.io);
        if io.running == 0 {
            return Err(kAudioHardwareNotRunningError);
        }
        Ok(io.clock.zero_timestamp(unsafe { mach_absolute_time() }))
    }

    pub fn will_do(op: u32) -> (bool, bool) {
        match op {
            kAudioServerPlugInIOOperationReadInput | kAudioServerPlugInIOOperationWriteMix => (true, true),
            _ => (false, true),
        }
    }

    /// ReadInput fills `buf` from the ring at the input time; WriteMix
    /// stores it (with the volume applied) at the output time.
    ///
    /// # Safety
    /// `buf` must hold `frames` interleaved frames of the device's channel
    /// count.
    pub unsafe fn do_io(
        &self,
        device: AudioObjectID,
        stream: AudioObjectID,
        op: u32,
        frames: u32,
        cycle: &AudioServerPlugInIOCycleInfo,
        buf: *mut c_void,
    ) -> Result<(), OSStatus> {
        let slot = self.device_slot(device)?;
        let Some(Obj::Device(s)) = Obj::from_id(device) else { return Err(kAudioHardwareBadObjectError) };
        let stream_obj = Obj::from_id(stream);
        if buf.is_null() {
            return Ok(());
        }
        let mut io = lock(&slot.io);
        let ch = io.ring.channels();
        let samples = std::slice::from_raw_parts_mut(buf as *mut f32, frames as usize * ch);
        match op {
            kAudioServerPlugInIOOperationReadInput => {
                if stream_obj != Some(Obj::InputStream(s)) {
                    return Err(kAudioHardwareBadStreamError);
                }
                io.ring.read(cycle.mInputTime.mSampleTime.floor() as i64, samples);
            }
            kAudioServerPlugInIOOperationWriteMix => {
                if stream_obj != Some(Obj::OutputStream(s)) {
                    return Err(kAudioHardwareBadStreamError);
                }
                let gain = volume::gain(f32::from_bits(slot.volume.load(Ordering::Relaxed)), slot.muted.load(Ordering::Relaxed));
                io.ring.write(cycle.mOutputTime.mSampleTime.floor() as i64, samples, gain);
            }
            _ => {}
        }
        Ok(())
    }

    // ---- host -------------------------------------------------------------

    fn host(&self) -> AudioServerPlugInHostRef {
        self.host.load(Ordering::Acquire)
    }

    /// Run host callbacks (no locks held).
    pub fn run_actions(&self, actions: Vec<HostAction>) {
        let host = self.host();
        if host.is_null() {
            return;
        }
        for a in actions {
            unsafe {
                match a {
                    HostAction::Changed(obj, addrs) => {
                        ((*host).PropertiesChanged)(host, obj, addrs.len() as u32, addrs.as_ptr());
                    }
                    HostAction::RequestConfigChange(dev) => {
                        ((*host).RequestDeviceConfigurationChange)(host, dev, 0, std::ptr::null_mut());
                    }
                    HostAction::Store(json) => {
                        let key = CFString::new(STORAGE_KEY);
                        let val = CFString::new(&json);
                        ((*host).WriteToStorage)(host, key.as_concrete_TypeRef(), val.as_concrete_TypeRef() as CFPropertyListRef);
                    }
                }
            }
        }
    }

    /// The stored configuration, if any and valid.
    fn stored_config(&self) -> Option<DriverConfig> {
        let host = self.host();
        if host.is_null() {
            return None;
        }
        let key = CFString::new(STORAGE_KEY);
        let mut out: CFPropertyListRef = std::ptr::null();
        let st = unsafe { ((*host).CopyFromStorage)(host, key.as_concrete_TypeRef(), &mut out) };
        if st != 0 || out.is_null() {
            return None;
        }
        let json = cfstring_to_string(out as CFStringRef);
        unsafe { CFRelease(out) };
        DriverConfig::from_json(&json?).ok()
    }

    pub fn initialize(&self, host: AudioServerPlugInHostRef) -> OSStatus {
        self.host.store(host as *mut _, Ordering::Release);
        if lock(&self.state).initialized {
            return kAudioHardwareNoError;
        }
        let cfg = self.stored_config().unwrap_or_default();
        self.apply_config(&cfg, true);
        kAudioHardwareNoError
    }
}

/// A CFString's contents (`None` for null or a non-string).
pub(crate) fn cfstring_to_string(r: CFStringRef) -> Option<String> {
    if r.is_null() || unsafe { CFGetTypeID(r as *const c_void) != CFStringGetTypeID() } {
        return None;
    }
    Some(unsafe { CFString::wrap_under_get_rule(r) }.to_string())
}

// ---- the COM object ------------------------------------------------------------

/// `AudioServerPlugInDriverRef` points at this: the vtable pointer first,
/// then our state.
#[repr(C)]
pub struct DriverObject {
    vtable: &'static AudioServerPlugInDriverInterface,
    pub driver: Driver,
}

impl DriverObject {
    pub fn new() -> Box<DriverObject> {
        Box::new(DriverObject { vtable: &VTABLE, driver: Driver::new() })
    }

    /// The `AudioServerPlugInDriverRef` for this object.
    pub fn as_ref_ptr(&self) -> AudioServerPlugInDriverRef {
        self as *const DriverObject as *mut c_void
    }
}

unsafe fn drv<'a>(p: *mut c_void) -> Option<&'a Driver> {
    (p as *const DriverObject).as_ref().map(|o| &o.driver)
}

/// Run `f` and turn a panic into an error (never unwind into the host).
fn guard(f: impl FnOnce() -> OSStatus) -> OSStatus {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(kAudioHardwareUnspecifiedError)
}

fn status(r: Result<(), OSStatus>) -> OSStatus {
    r.err().unwrap_or(kAudioHardwareNoError)
}

unsafe extern "C" fn query_interface(p: *mut c_void, uuid: CFUUIDBytes, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return E_NOINTERFACE;
    }
    let id = uuid_bytes(&uuid);
    match drv(p) {
        Some(d) if id == IUNKNOWN_UUID_BYTES || id == DRIVER_INTERFACE_UUID_BYTES => {
            d.refs.fetch_add(1, Ordering::AcqRel);
            *out = p;
            S_OK
        }
        _ => {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "C" fn add_ref(p: *mut c_void) -> ULONG {
    drv(p).map_or(0, |d| d.refs.fetch_add(1, Ordering::AcqRel).saturating_add(1))
}

/// The object lives for the life of the process (the host never unloads
/// a HAL driver); the count is kept for COM's sake.
unsafe extern "C" fn release(p: *mut c_void) -> ULONG {
    drv(p).map_or(0, |d| {
        let prev = d.refs.load(Ordering::Acquire);
        if prev > 0 {
            d.refs.fetch_sub(1, Ordering::AcqRel) - 1
        } else {
            0
        }
    })
}

unsafe extern "C" fn initialize(p: AudioServerPlugInDriverRef, host: AudioServerPlugInHostRef) -> OSStatus {
    guard(|| drv(p).map_or(kAudioHardwareBadObjectError, |d| d.initialize(host)))
}

unsafe extern "C" fn create_device(
    _: AudioServerPlugInDriverRef,
    _: CFDictionaryRef,
    _: *const AudioServerPlugInClientInfo,
    _: *mut AudioObjectID,
) -> OSStatus {
    kAudioHardwareUnsupportedOperationError
}

unsafe extern "C" fn destroy_device(_: AudioServerPlugInDriverRef, _: AudioObjectID) -> OSStatus {
    kAudioHardwareUnsupportedOperationError
}

unsafe extern "C" fn add_device_client(_: AudioServerPlugInDriverRef, _: AudioObjectID, _: *const AudioServerPlugInClientInfo) -> OSStatus {
    kAudioHardwareNoError
}

unsafe extern "C" fn remove_device_client(_: AudioServerPlugInDriverRef, _: AudioObjectID, _: *const AudioServerPlugInClientInfo) -> OSStatus {
    kAudioHardwareNoError
}

unsafe extern "C" fn perform_config_change(p: AudioServerPlugInDriverRef, dev: AudioObjectID, _: u64, _: *mut c_void) -> OSStatus {
    guard(|| {
        let Some(d) = drv(p) else { return kAudioHardwareBadObjectError };
        match d.perform_config_change(dev) {
            Ok(actions) => {
                d.run_actions(actions);
                kAudioHardwareNoError
            }
            Err(e) => e,
        }
    })
}

unsafe extern "C" fn abort_config_change(p: AudioServerPlugInDriverRef, dev: AudioObjectID, _: u64, _: *mut c_void) -> OSStatus {
    guard(|| {
        if let Some(d) = drv(p) {
            d.abort_config_change(dev);
        }
        kAudioHardwareNoError
    })
}

unsafe extern "C" fn has_property(p: AudioServerPlugInDriverRef, id: AudioObjectID, _: pid_t, addr: *const AudioObjectPropertyAddress) -> Boolean {
    catch_unwind(AssertUnwindSafe(|| match (drv(p), addr.as_ref()) {
        (Some(d), Some(a)) => d.has(id, a) as Boolean,
        _ => 0,
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn is_property_settable(
    p: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _: pid_t,
    addr: *const AudioObjectPropertyAddress,
    out: *mut Boolean,
) -> OSStatus {
    guard(|| {
        let (Some(d), Some(a)) = (drv(p), addr.as_ref()) else { return kAudioHardwareIllegalOperationError };
        if out.is_null() {
            return kAudioHardwareIllegalOperationError;
        }
        match d.settable(id, a) {
            Ok(s) => {
                *out = s as Boolean;
                kAudioHardwareNoError
            }
            Err(e) => e,
        }
    })
}

unsafe fn bytes<'a>(p: *const c_void, n: u32) -> &'a [u8] {
    if p.is_null() || n == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(p as *const u8, n as usize)
    }
}

unsafe extern "C" fn get_property_data_size(
    p: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _: pid_t,
    addr: *const AudioObjectPropertyAddress,
    qsize: u32,
    qdata: *const c_void,
    out: *mut u32,
) -> OSStatus {
    guard(|| {
        let (Some(d), Some(a)) = (drv(p), addr.as_ref()) else { return kAudioHardwareIllegalOperationError };
        if out.is_null() {
            return kAudioHardwareIllegalOperationError;
        }
        match d.get(id, a, bytes(qdata, qsize), None) {
            Ok(v) => {
                *out = v.size();
                kAudioHardwareNoError
            }
            Err(e) => e,
        }
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn get_property_data(
    p: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _: pid_t,
    addr: *const AudioObjectPropertyAddress,
    qsize: u32,
    qdata: *const c_void,
    size: u32,
    out_size: *mut u32,
    out: *mut c_void,
) -> OSStatus {
    guard(|| {
        let (Some(d), Some(a)) = (drv(p), addr.as_ref()) else { return kAudioHardwareIllegalOperationError };
        if out_size.is_null() {
            return kAudioHardwareIllegalOperationError;
        }
        let input = (size >= 4 && !out.is_null()).then(|| (out as *const f32).read_unaligned());
        match d.get(id, a, bytes(qdata, qsize), input).and_then(|v| v.write(out, size)) {
            Ok(n) => {
                *out_size = n;
                kAudioHardwareNoError
            }
            Err(e) => e,
        }
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn set_property_data(
    p: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _: pid_t,
    addr: *const AudioObjectPropertyAddress,
    _qsize: u32,
    _qdata: *const c_void,
    size: u32,
    data: *const c_void,
) -> OSStatus {
    guard(|| {
        let (Some(d), Some(a)) = (drv(p), addr.as_ref()) else { return kAudioHardwareIllegalOperationError };
        match d.set(id, a, bytes(data, size)) {
            Ok(actions) => {
                d.run_actions(actions);
                kAudioHardwareNoError
            }
            Err(e) => e,
        }
    })
}

unsafe extern "C" fn start_io(p: AudioServerPlugInDriverRef, dev: AudioObjectID, _: u32) -> OSStatus {
    guard(|| drv(p).map_or(kAudioHardwareBadObjectError, |d| status(d.start_io(dev))))
}

unsafe extern "C" fn stop_io(p: AudioServerPlugInDriverRef, dev: AudioObjectID, _: u32) -> OSStatus {
    guard(|| drv(p).map_or(kAudioHardwareBadObjectError, |d| status(d.stop_io(dev))))
}

unsafe extern "C" fn get_zero_timestamp(
    p: AudioServerPlugInDriverRef,
    dev: AudioObjectID,
    _: u32,
    sample: *mut f64,
    host: *mut u64,
    seed: *mut u64,
) -> OSStatus {
    guard(|| {
        let Some(d) = drv(p) else { return kAudioHardwareBadObjectError };
        if sample.is_null() || host.is_null() || seed.is_null() {
            return kAudioHardwareIllegalOperationError;
        }
        match d.zero_timestamp(dev) {
            Ok((s, h, sd)) => {
                *sample = s;
                *host = h;
                *seed = sd;
                kAudioHardwareNoError
            }
            Err(e) => e,
        }
    })
}

unsafe extern "C" fn will_do_io_operation(
    _: AudioServerPlugInDriverRef,
    _: AudioObjectID,
    _: u32,
    op: u32,
    will_do: *mut Boolean,
    in_place: *mut Boolean,
) -> OSStatus {
    let (w, ip) = Driver::will_do(op);
    if !will_do.is_null() {
        *will_do = w as Boolean;
    }
    if !in_place.is_null() {
        *in_place = ip as Boolean;
    }
    kAudioHardwareNoError
}

unsafe extern "C" fn begin_io_operation(
    _: AudioServerPlugInDriverRef,
    _: AudioObjectID,
    _: u32,
    _: u32,
    _: u32,
    _: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    kAudioHardwareNoError
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn do_io_operation(
    p: AudioServerPlugInDriverRef,
    dev: AudioObjectID,
    stream: AudioObjectID,
    _: u32,
    op: u32,
    frames: u32,
    cycle: *const AudioServerPlugInIOCycleInfo,
    main: *mut c_void,
    _secondary: *mut c_void,
) -> OSStatus {
    guard(|| {
        let (Some(d), Some(c)) = (drv(p), cycle.as_ref()) else { return kAudioHardwareIllegalOperationError };
        status(d.do_io(dev, stream, op, frames, c, main))
    })
}

unsafe extern "C" fn end_io_operation(
    _: AudioServerPlugInDriverRef,
    _: AudioObjectID,
    _: u32,
    _: u32,
    _: u32,
    _: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    kAudioHardwareNoError
}

pub static VTABLE: AudioServerPlugInDriverInterface = AudioServerPlugInDriverInterface {
    _reserved: std::ptr::null_mut(),
    QueryInterface: query_interface,
    AddRef: add_ref,
    Release: release,
    Initialize: initialize,
    CreateDevice: create_device,
    DestroyDevice: destroy_device,
    AddDeviceClient: add_device_client,
    RemoveDeviceClient: remove_device_client,
    PerformDeviceConfigurationChange: perform_config_change,
    AbortDeviceConfigurationChange: abort_config_change,
    HasProperty: has_property,
    IsPropertySettable: is_property_settable,
    GetPropertyDataSize: get_property_data_size,
    GetPropertyData: get_property_data,
    SetPropertyData: set_property_data,
    StartIO: start_io,
    StopIO: stop_io,
    GetZeroTimeStamp: get_zero_timestamp,
    WillDoIOOperation: will_do_io_operation,
    BeginIOOperation: begin_io_operation,
    DoIOOperation: do_io_operation,
    EndIOOperation: end_io_operation,
};

struct GlobalPtr(*mut DriverObject);
unsafe impl Send for GlobalPtr {}
unsafe impl Sync for GlobalPtr {}

/// The CFPlugIn factory named in Info.plist (`CFPlugInFactories`). Returns
/// the one driver object for `kAudioServerPlugInTypeUUID`, else null.
///
/// # Safety
/// Called by CFPlugIn with a valid (or null) type UUID.
#[no_mangle]
pub unsafe extern "C" fn PowerMacAudio_Create(_alloc: CFAllocatorRef, requested_type: CFUUIDRef) -> *mut c_void {
    catch_unwind(|| {
        if requested_type.is_null() || uuid_bytes(&CFUUIDGetUUIDBytes(requested_type)) != PLUGIN_TYPE_UUID_BYTES {
            return std::ptr::null_mut();
        }
        static GLOBAL: OnceLock<GlobalPtr> = OnceLock::new();
        let obj = GLOBAL.get_or_init(|| GlobalPtr(Box::into_raw(DriverObject::new()))).0;
        obj as *mut c_void
    })
    .unwrap_or(std::ptr::null_mut())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver_with(cfg: &DriverConfig) -> Driver {
        let d = Driver::new();
        d.apply_config(cfg, true);
        d
    }

    fn addr(sel: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress::global(sel)
    }

    fn scoped(sel: u32, scope: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress::new(sel, scope, 0)
    }

    #[test]
    fn object_ids_round_trip() {
        for o in [Obj::PlugIn, Obj::Device(0), Obj::InputStream(3), Obj::OutputStream(15), Obj::Volume(7), Obj::Mute(1)] {
            assert_eq!(Obj::from_id(o.id()), Some(o));
        }
        assert_eq!(Obj::from_id(0), None);
        assert_eq!(Obj::from_id(2), None);
        assert_eq!(Obj::from_id(FIRST_DEVICE_ID + 5), None);
        assert_eq!(Obj::from_id(FIRST_DEVICE_ID + MAX_DEVICES as u32 * OBJECTS_PER_DEVICE), None);
    }

    #[test]
    fn plugin_properties() {
        let d = driver_with(&DriverConfig::with_mixes(2, 48_000.0, 2));
        let p = kAudioObjectPlugInObject;
        assert_eq!(d.get(p, &addr(kAudioObjectPropertyClass), &[], None), Ok(Value::U32(kAudioPlugInClassID)));
        let devs = d.device_ids();
        assert_eq!(devs, vec![16, 24, 32]);
        assert_eq!(d.get(p, &addr(kAudioPlugInPropertyDeviceList), &[], None), Ok(Value::Ids(devs.clone())));
        // OwnedObjects filtered by class.
        let q = kAudioStreamClassID.to_ne_bytes();
        assert_eq!(d.get(p, &addr(kAudioObjectPropertyOwnedObjects), &q, None), Ok(Value::Ids(vec![])));
        let q = kAudioDeviceClassID.to_ne_bytes();
        assert_eq!(d.get(p, &addr(kAudioObjectPropertyOwnedObjects), &q, None), Ok(Value::Ids(devs)));
        assert!(d.has(p, &addr(kAudioObjectPropertyCustomPropertyInfoList)));
        assert!(d.settable(p, &addr(CONFIG_PROPERTY)).unwrap());
        assert!(!d.settable(p, &addr(kAudioPlugInPropertyDeviceList)).unwrap());
        assert_eq!(d.get(p, &addr(fourcc(b"zzzz")), &[], None), Err(kAudioHardwareUnknownPropertyError));
        match d.get(p, &addr(CONFIG_PROPERTY), &[], None).unwrap() {
            Value::Str(json) => assert_eq!(DriverConfig::from_json(&json).unwrap(), d.config()),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn translate_uid() {
        let d = driver_with(&DriverConfig::with_mixes(2, 48_000.0, 1));
        let uid = CFString::new(&mix_uid(1));
        let q = (uid.as_concrete_TypeRef() as usize).to_ne_bytes();
        let a = addr(kAudioPlugInPropertyTranslateUIDToDevice);
        assert_eq!(d.get(1, &a, &q, None), Ok(Value::U32(24)));
        let other = CFString::new("nope");
        let q = (other.as_concrete_TypeRef() as usize).to_ne_bytes();
        assert_eq!(d.get(1, &a, &q, None), Ok(Value::U32(0)));
        assert!(d.has(1, &a));
    }

    #[test]
    fn device_property_table() {
        let d = driver_with(&DriverConfig::with_mixes(8, 96_000.0, 0));
        let dev = 16;
        let g = |sel| d.get(dev, &addr(sel), &[], None).unwrap();
        assert_eq!(g(kAudioObjectPropertyClass), Value::U32(kAudioDeviceClassID));
        assert_eq!(g(kAudioObjectPropertyOwner), Value::U32(kAudioObjectPlugInObject));
        assert_eq!(g(kAudioObjectPropertyName), Value::Str("PowerMac Audio".into()));
        assert_eq!(g(kAudioDevicePropertyDeviceUID), Value::Str(MAIN_UID.into()));
        assert_eq!(g(kAudioDevicePropertyTransportType), Value::U32(kAudioDeviceTransportTypeVirtual));
        assert_eq!(g(kAudioDevicePropertyNominalSampleRate), Value::F64(96_000.0));
        assert_eq!(g(kAudioDevicePropertyLatency), Value::U32(0));
        assert_eq!(g(kAudioDevicePropertySafetyOffset), Value::U32(0));
        assert_eq!(g(kAudioDevicePropertyZeroTimeStampPeriod), Value::U32(RING_FRAMES));
        assert_eq!(g(kAudioDevicePropertyDeviceIsRunning), Value::U32(0));
        assert_eq!(g(kAudioObjectPropertyControlList), Value::Ids(vec![19, 20]));
        assert_eq!(g(kAudioDevicePropertyAvailableNominalSampleRates).size(), 3 * 16);
        let streams = |scope| d.get(dev, &scoped(kAudioDevicePropertyStreams, scope), &[], None).unwrap();
        assert_eq!(streams(kAudioObjectPropertyScopeInput), Value::Ids(vec![17]));
        assert_eq!(streams(kAudioObjectPropertyScopeOutput), Value::Ids(vec![18]));
        assert_eq!(streams(kAudioObjectPropertyScopeGlobal), Value::Ids(vec![17, 18]));
        let layout = g(kAudioDevicePropertyPreferredChannelLayout);
        assert_eq!(layout.size(), 12 + 8 * 20);
        if let Value::Pod(b, _) = layout {
            assert_eq!(u32::from_ne_bytes(b[8..12].try_into().unwrap()), 8);
            assert_eq!(u32::from_ne_bytes(b[12..16].try_into().unwrap()), kAudioChannelLabel_Discrete_0);
        }
        // Owned objects in the output scope, filtered to controls.
        let q = kAudioControlClassID.to_ne_bytes();
        assert_eq!(d.get(dev, &scoped(kAudioObjectPropertyOwnedObjects, kAudioObjectPropertyScopeOutput), &q, None), Ok(Value::Ids(vec![19, 20])));
        let q = kAudioLevelControlClassID.to_ne_bytes();
        assert_eq!(d.get(dev, &addr(kAudioObjectPropertyOwnedObjects), &q, None), Ok(Value::Ids(vec![19])));
        // A slot without a device is a bad object.
        assert_eq!(d.get(24, &addr(kAudioObjectPropertyName), &[], None), Err(kAudioHardwareBadObjectError));
    }

    #[test]
    fn stream_and_control_tables() {
        let d = driver_with(&DriverConfig::with_mixes(16, 44_100.0, 0));
        let fmt = d.get(18, &addr(kAudioStreamPropertyVirtualFormat), &[], None).unwrap();
        assert_eq!(fmt, pod_whole(&stream_format(44_100.0, 16)));
        assert_eq!(d.get(17, &addr(kAudioStreamPropertyDirection), &[], None), Ok(Value::U32(1)));
        assert_eq!(d.get(18, &addr(kAudioStreamPropertyDirection), &[], None), Ok(Value::U32(0)));
        assert_eq!(d.get(18, &addr(kAudioStreamPropertyAvailablePhysicalFormats), &[], None).unwrap().size(), 3 * 56);
        assert_eq!(d.get(19, &addr(kAudioObjectPropertyBaseClass), &[], None), Ok(Value::U32(kAudioLevelControlClassID)));
        assert_eq!(d.get(19, &addr(kAudioObjectPropertyOwner), &[], None), Ok(Value::U32(16)));
        assert_eq!(d.get(20, &addr(kAudioObjectPropertyClass), &[], None), Ok(Value::U32(kAudioMuteControlClassID)));
        assert_eq!(d.get(19, &addr(kAudioLevelControlPropertyConvertScalarToDecibels), &[], Some(1.0)), Ok(Value::F32(0.0)));
        assert!(d.settable(19, &addr(kAudioLevelControlPropertyScalarValue)).unwrap());
        assert!(d.settable(17, &addr(kAudioStreamPropertyIsActive)).unwrap());
        assert!(!d.settable(17, &addr(kAudioStreamPropertyDirection)).unwrap());
    }

    #[test]
    fn volume_and_mute_set_and_notify() {
        let d = driver_with(&DriverConfig::default());
        let a = d.set(19, &addr(kAudioLevelControlPropertyScalarValue), &0.5f32.to_ne_bytes()).unwrap();
        assert!(matches!(&a[..], [HostAction::Changed(19, _)]));
        assert_eq!(d.get(19, &addr(kAudioLevelControlPropertyScalarValue), &[], None), Ok(Value::F32(0.5)));
        assert!(d.set(19, &addr(kAudioLevelControlPropertyScalarValue), &0.5f32.to_ne_bytes()).unwrap().is_empty());
        d.set(19, &addr(kAudioLevelControlPropertyDecibelValue), &0.0f32.to_ne_bytes()).unwrap();
        assert_eq!(d.get(19, &addr(kAudioLevelControlPropertyScalarValue), &[], None), Ok(Value::F32(1.0)));
        d.set(20, &addr(kAudioBooleanControlPropertyValue), &1u32.to_ne_bytes()).unwrap();
        assert_eq!(d.get(20, &addr(kAudioBooleanControlPropertyValue), &[], None), Ok(Value::U32(1)));
        assert_eq!(d.set(16, &addr(kAudioObjectPropertyName), b"x"), Err(kAudioHardwareUnsupportedOperationError));
        assert_eq!(d.set(19, &addr(kAudioLevelControlPropertyScalarValue), &[0]), Err(kAudioHardwareBadPropertySizeError));
    }

    #[test]
    fn sample_rate_change_goes_through_the_host() {
        let d = driver_with(&DriverConfig::default());
        let a = d.set(16, &addr(kAudioDevicePropertyNominalSampleRate), &96_000f64.to_ne_bytes()).unwrap();
        assert_eq!(a, vec![HostAction::RequestConfigChange(16)]);
        // Not applied until the host says so.
        assert_eq!(d.get(16, &addr(kAudioDevicePropertyNominalSampleRate), &[], None), Ok(Value::F64(48_000.0)));
        assert_eq!(d.config().devices[0].sample_rate, 96_000.0);
        let a = d.perform_config_change(16).unwrap();
        assert!(matches!(&a[..], [HostAction::Store(j)] if j.contains("96000")));
        assert_eq!(d.get(16, &addr(kAudioDevicePropertyNominalSampleRate), &[], None), Ok(Value::F64(96_000.0)));
        assert_eq!(
            d.set(16, &addr(kAudioDevicePropertyNominalSampleRate), &22_050f64.to_ne_bytes()),
            Err(kAudioDeviceUnsupportedFormatError)
        );
        // Through the stream format, and aborting.
        let f = stream_format(44_100.0, 2);
        let raw = unsafe { std::slice::from_raw_parts(&f as *const _ as *const u8, size_of::<AudioStreamBasicDescription>()) };
        assert_eq!(d.set(18, &addr(kAudioStreamPropertyVirtualFormat), raw).unwrap(), vec![HostAction::RequestConfigChange(16)]);
        d.abort_config_change(16);
        assert!(d.perform_config_change(16).unwrap().is_empty());
        assert_eq!(d.get(16, &addr(kAudioDevicePropertyNominalSampleRate), &[], None), Ok(Value::F64(96_000.0)));
        let bad = stream_format(48_000.0, 8);
        let raw = unsafe { std::slice::from_raw_parts(&bad as *const _ as *const u8, size_of::<AudioStreamBasicDescription>()) };
        assert_eq!(d.set(18, &addr(kAudioStreamPropertyVirtualFormat), raw), Err(kAudioDeviceUnsupportedFormatError));
    }

    #[test]
    fn config_changes_add_remove_rename_and_reformat() {
        let d = driver_with(&DriverConfig::default());
        let mut cfg = DriverConfig::with_mixes(2, 48_000.0, 2);
        let a = d.apply_config(&cfg, false);
        assert!(a.iter().any(|x| matches!(x, HostAction::Changed(1, v) if v.len() == 2)));
        assert!(matches!(a.last(), Some(HostAction::Store(_))));
        assert_eq!(d.device_ids(), vec![16, 24, 32]);
        // Remove mix 1: mix 2 keeps its ids.
        cfg.devices.remove(1);
        d.apply_config(&cfg, false);
        assert_eq!(d.device_ids(), vec![16, 32]);
        assert_eq!(d.get(24, &addr(kAudioObjectPropertyName), &[], None), Err(kAudioHardwareBadObjectError));
        // Rename: a notification, no reconfiguration.
        cfg.devices[1].name = "Stream".into();
        let a = d.apply_config(&cfg, false);
        assert_eq!(a[0], HostAction::Changed(32, vec![addr(kAudioObjectPropertyName)]));
        assert!(!a.iter().any(|x| matches!(x, HostAction::RequestConfigChange(_))));
        // A new mix reuses the free slot.
        cfg.devices.push(DeviceSpec::new("x", "X", 2, 48_000.0));
        d.apply_config(&cfg, false);
        assert_eq!(d.device_ids(), vec![16, 32, 24]);
        // Channel change: requested, applied on perform, ring resized.
        cfg.devices[0].channels = 64;
        let a = d.apply_config(&cfg, false);
        assert!(a.contains(&HostAction::RequestConfigChange(16)));
        d.perform_config_change(16).unwrap();
        assert_eq!(lock(&d.slots[0].io).ring.channels(), 64);
        assert_eq!(d.get(18, &addr(kAudioStreamPropertyVirtualFormat), &[], None), Ok(pod_whole(&stream_format(48_000.0, 64))));
    }

    #[test]
    fn io_loopback_through_the_driver() {
        let d = driver_with(&DriverConfig::default());
        assert_eq!(d.zero_timestamp(16), Err(kAudioHardwareNotRunningError));
        d.start_io(16).unwrap();
        let mut cycle = AudioServerPlugInIOCycleInfo::default();
        cycle.mOutputTime.mSampleTime = 1024.0;
        let mut out: Vec<f32> = (0..512).map(|i| i as f32 / 512.0).collect();
        let expect = out.clone();
        unsafe { d.do_io(16, 18, kAudioServerPlugInIOOperationWriteMix, 256, &cycle, out.as_mut_ptr() as *mut c_void).unwrap() };
        cycle.mInputTime.mSampleTime = 1024.0;
        let mut inp = vec![0f32; 512];
        unsafe { d.do_io(16, 17, kAudioServerPlugInIOOperationReadInput, 256, &cycle, inp.as_mut_ptr() as *mut c_void).unwrap() };
        assert_eq!(inp, expect);
        // Wrong stream for the operation.
        assert_eq!(
            unsafe { d.do_io(16, 18, kAudioServerPlugInIOOperationReadInput, 256, &cycle, inp.as_mut_ptr() as *mut c_void) },
            Err(kAudioHardwareBadStreamError)
        );
        // Muted output records silence.
        d.set(20, &addr(kAudioBooleanControlPropertyValue), &1u32.to_ne_bytes()).unwrap();
        cycle.mOutputTime.mSampleTime = 2048.0;
        unsafe { d.do_io(16, 18, kAudioServerPlugInIOOperationWriteMix, 256, &cycle, out.as_mut_ptr() as *mut c_void).unwrap() };
        cycle.mInputTime.mSampleTime = 2048.0;
        unsafe { d.do_io(16, 17, kAudioServerPlugInIOOperationReadInput, 256, &cycle, inp.as_mut_ptr() as *mut c_void).unwrap() };
        assert!(inp.iter().all(|&s| s == 0.0));
        let (s, h, _) = d.zero_timestamp(16).unwrap();
        assert_eq!(s % RING_FRAMES as f64, 0.0);
        assert!(h <= unsafe { mach_absolute_time() });
        assert_eq!(d.get(16, &addr(kAudioDevicePropertyDeviceIsRunning), &[], None), Ok(Value::U32(1)));
        d.stop_io(16).unwrap();
        assert_eq!(d.stop_io(16), Err(kAudioHardwareNotRunningError));
        assert_eq!(d.start_io(24), Err(kAudioHardwareBadObjectError));
    }

    #[test]
    fn value_write_rules() {
        let mut buf = [0u8; 8];
        unsafe {
            assert_eq!(Value::U32(7).write(buf.as_mut_ptr() as *mut c_void, 2), Err(kAudioHardwareBadPropertySizeError));
            assert_eq!(Value::Ids(vec![1, 2, 3]).write(buf.as_mut_ptr() as *mut c_void, 8), Ok(8));
            assert_eq!(Value::Pod(vec![1; 12], 0).write(buf.as_mut_ptr() as *mut c_void, 8), Err(kAudioHardwareBadPropertySizeError));
            assert_eq!(Value::Pod(vec![1; 12], 4).write(buf.as_mut_ptr() as *mut c_void, 8), Ok(8));
            assert_eq!(Value::Ids(vec![]).write(std::ptr::null_mut(), 0), Ok(0));
        }
    }

    #[test]
    fn factory_rejects_other_types() {
        use core_foundation_sys::uuid::CFUUIDGetConstantUUIDWithBytes;
        unsafe {
            let b = PLUGIN_TYPE_UUID_BYTES;
            let t = CFUUIDGetConstantUUIDWithBytes(
                std::ptr::null(),
                b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
            );
            let p = PowerMacAudio_Create(std::ptr::null(), t);
            assert!(!p.is_null());
            assert_eq!(p, PowerMacAudio_Create(std::ptr::null(), t), "one driver object per process");
            let other = CFUUIDGetConstantUUIDWithBytes(std::ptr::null(), 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);
            assert!(PowerMacAudio_Create(std::ptr::null(), other).is_null());
            assert!(PowerMacAudio_Create(std::ptr::null(), std::ptr::null()).is_null());
        }
    }
}
