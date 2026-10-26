//! macOS backend: CoreAudio devices, defaults, volume, notifications and
//! (macOS 14.4+) per-app volume through process taps.

pub(crate) mod ffi;
pub(crate) mod props;
pub(crate) mod tap;
pub(crate) mod tcc;
pub mod testing;

use crate::rules::Snapshot;
use crate::{AppLevel, AudioApp, AudioDevice, Backend, CapturePermission, DeviceKind, Transport};
use anyhow::{anyhow, Result};
use ffi::*;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

/// Our private aggregate devices carry this uid prefix and are hidden.
pub(crate) const AGGREGATE_UID_PREFIX: &str = "com.powermac.audio-control.";

pub(crate) fn per_app_volume_supported() -> bool {
    tap::supported()
}

#[allow(non_upper_case_globals)]
fn transport_of(code: u32) -> Transport {
    match code {
        kAudioDeviceTransportTypeBuiltIn => Transport::BuiltIn,
        kAudioDeviceTransportTypeUSB => Transport::Usb,
        kAudioDeviceTransportTypeBluetooth => Transport::Bluetooth,
        kAudioDeviceTransportTypeBluetoothLE => Transport::BluetoothLe,
        kAudioDeviceTransportTypeHDMI => Transport::Hdmi,
        kAudioDeviceTransportTypeDisplayPort => Transport::DisplayPort,
        kAudioDeviceTransportTypeAirPlay => Transport::AirPlay,
        kAudioDeviceTransportTypeVirtual => Transport::Virtual,
        kAudioDeviceTransportTypeAggregate => Transport::Aggregate,
        kAudioDeviceTransportTypeThunderbolt => Transport::Thunderbolt,
        kAudioDeviceTransportTypePCI => Transport::Pci,
        kAudioDeviceTransportTypeFireWire | kAudioDeviceTransportTypeAVB => Transport::FireWire,
        kAudioDeviceTransportTypeContinuityCaptureWired | kAudioDeviceTransportTypeContinuityCaptureWireless => {
            Transport::Continuity
        }
        _ => Transport::Unknown,
    }
}

fn scope_of(kind: DeviceKind) -> u32 {
    match kind {
        DeviceKind::Input => kAudioObjectPropertyScopeInput,
        _ => kAudioObjectPropertyScopeOutput,
    }
}

fn default_selector(kind: DeviceKind) -> u32 {
    match kind {
        DeviceKind::Output => kAudioHardwarePropertyDefaultOutputDevice,
        DeviceKind::Input => kAudioHardwarePropertyDefaultInputDevice,
        DeviceKind::SystemOutput => kAudioHardwarePropertyDefaultSystemOutputDevice,
    }
}

pub(crate) fn device_ids() -> Vec<AudioObjectID> {
    props::get_ids(kAudioObjectSystemObject, AudioObjectPropertyAddress::global(kAudioHardwarePropertyDevices))
        .unwrap_or_default()
}

pub(crate) fn device_uid(id: AudioObjectID) -> Option<String> {
    props::get_string(id, AudioObjectPropertyAddress::global(kAudioDevicePropertyDeviceUID)).ok()
}

pub(crate) fn default_device_id(kind: DeviceKind) -> AudioObjectID {
    props::get::<u32>(kAudioObjectSystemObject, AudioObjectPropertyAddress::global(default_selector(kind)))
        .unwrap_or(kAudioObjectUnknown)
}

pub(crate) fn id_for_uid(uid: &str) -> Option<AudioObjectID> {
    device_ids().into_iter().find(|&id| device_uid(id).as_deref() == Some(uid))
}

pub(crate) fn default_output_uid() -> Option<String> {
    let id = default_device_id(DeviceKind::Output);
    (id != kAudioObjectUnknown).then(|| device_uid(id)).flatten()
}

/// Volume of a device in `scope`: the main element, else the average of the
/// (stereo) channels.
fn volume(id: AudioObjectID, scope: u32, channels: u32) -> Option<f32> {
    let main = AudioObjectPropertyAddress::new(kAudioDevicePropertyVolumeScalar, scope, kAudioObjectPropertyElementMain);
    if props::has(id, main) {
        return props::get::<f32>(id, main).ok();
    }
    let vals: Vec<f32> = volume_channels(id, scope, channels)
        .into_iter()
        .filter_map(|ch| props::get::<f32>(id, AudioObjectPropertyAddress::new(kAudioDevicePropertyVolumeScalar, scope, ch)).ok())
        .collect();
    (!vals.is_empty()).then(|| vals.iter().sum::<f32>() / vals.len() as f32)
}

fn volume_channels(id: AudioObjectID, scope: u32, channels: u32) -> Vec<u32> {
    let mut chans: Vec<u32> = Vec::new();
    if scope == kAudioObjectPropertyScopeOutput {
        if let Ok(pair) = props::get::<[u32; 2]>(id, AudioObjectPropertyAddress::new(kAudioDevicePropertyPreferredChannelsForStereo, scope, 0)) {
            chans.extend(pair.iter().filter(|c| **c > 0));
        }
    }
    if chans.is_empty() {
        chans = (1..=channels.clamp(1, 2)).collect();
    }
    chans.retain(|&ch| props::has(id, AudioObjectPropertyAddress::new(kAudioDevicePropertyVolumeScalar, scope, ch)));
    chans
}

fn set_volume(id: AudioObjectID, scope: u32, channels: u32, v: f32) -> Result<()> {
    let main = AudioObjectPropertyAddress::new(kAudioDevicePropertyVolumeScalar, scope, kAudioObjectPropertyElementMain);
    if props::has(id, main) && props::settable(id, main) {
        return props::set::<f32>(id, main, v);
    }
    let chans = volume_channels(id, scope, channels);
    if chans.is_empty() {
        return Err(anyhow!("this device has no volume control"));
    }
    for ch in chans {
        props::set::<f32>(id, AudioObjectPropertyAddress::new(kAudioDevicePropertyVolumeScalar, scope, ch), v)?;
    }
    Ok(())
}

fn mute(id: AudioObjectID, scope: u32) -> Option<bool> {
    let main = AudioObjectPropertyAddress::new(kAudioDevicePropertyMute, scope, kAudioObjectPropertyElementMain);
    if props::has(id, main) {
        return props::get::<u32>(id, main).ok().map(|m| m != 0);
    }
    let a1 = AudioObjectPropertyAddress::new(kAudioDevicePropertyMute, scope, 1);
    props::has(id, a1).then(|| props::get::<u32>(id, a1).ok().map(|m| m != 0)).flatten()
}

fn set_mute(id: AudioObjectID, scope: u32, channels: u32, m: bool) -> Result<()> {
    let main = AudioObjectPropertyAddress::new(kAudioDevicePropertyMute, scope, kAudioObjectPropertyElementMain);
    if props::has(id, main) && props::settable(id, main) {
        return props::set::<u32>(id, main, m as u32);
    }
    let mut any = false;
    for ch in 1..=channels.max(1) {
        let a = AudioObjectPropertyAddress::new(kAudioDevicePropertyMute, scope, ch);
        if props::has(id, a) && props::settable(id, a) {
            props::set::<u32>(id, a, m as u32)?;
            any = true;
        }
    }
    if any {
        Ok(())
    } else {
        Err(anyhow!("this device has no mute control"))
    }
}

/// Whether the device may be the default for `scope` (property missing → yes).
fn can_be_default(id: AudioObjectID, scope: u32, system: bool) -> bool {
    let sel = if system { kAudioDevicePropertyDeviceCanBeDefaultSystemDevice } else { kAudioDevicePropertyDeviceCanBeDefaultDevice };
    let a = AudioObjectPropertyAddress::new(sel, scope, kAudioObjectPropertyElementMain);
    if !props::has(id, a) {
        return true;
    }
    props::get::<u32>(id, a).map(|v| v != 0).unwrap_or(true)
}

struct DevRaw {
    id: AudioObjectID,
    uid: String,
    name: String,
    transport: Transport,
    out_ch: u32,
    in_ch: u32,
}

fn read_device(id: AudioObjectID) -> Option<DevRaw> {
    let uid = device_uid(id)?;
    if uid.starts_with(AGGREGATE_UID_PREFIX) {
        return None;
    }
    if props::get::<u32>(id, AudioObjectPropertyAddress::global(kAudioDevicePropertyIsHidden)).unwrap_or(0) != 0 {
        return None;
    }
    let name = props::get_string(id, AudioObjectPropertyAddress::global(kAudioObjectPropertyName)).unwrap_or_default();
    let transport = transport_of(props::get::<u32>(id, AudioObjectPropertyAddress::global(kAudioDevicePropertyTransportType)).unwrap_or(0));
    Some(DevRaw {
        id,
        uid,
        name,
        transport,
        out_ch: props::channel_count(id, kAudioObjectPropertyScopeOutput),
        in_ch: props::channel_count(id, kAudioObjectPropertyScopeInput),
    })
}

pub(crate) struct MacBackend {
    taps: Mutex<tap::TapManager>,
}

impl MacBackend {
    pub(crate) fn new() -> Result<Self> {
        // Deliver HAL notifications on the HAL's own thread instead of the
        // main run loop (which a CLI or a busy UI thread may not run).
        let null: *const c_void = std::ptr::null();
        let _ = props::set(kAudioObjectSystemObject, AudioObjectPropertyAddress::global(kAudioHardwarePropertyRunLoop), null);
        Ok(MacBackend { taps: Mutex::new(tap::TapManager::default()) })
    }

    fn dev_for(&self, uid: &str) -> Result<DevRaw> {
        let id = id_for_uid(uid).ok_or_else(|| anyhow!("no audio device with uid {uid}"))?;
        read_device(id).ok_or_else(|| anyhow!("audio device {uid} is not available"))
    }
}

struct ListenerGuard {
    data: *mut c_void,
}

// The pointer is an `Arc<dyn Fn() + Send + Sync>` box.
unsafe impl Send for ListenerGuard {}

const WATCHED: [u32; 5] = [
    kAudioHardwarePropertyDevices,
    kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultSystemOutputDevice,
    kAudioHardwarePropertyProcessObjectList,
];

unsafe extern "C" fn listener(_: AudioObjectID, _: u32, _: *const AudioObjectPropertyAddress, data: *mut c_void) -> OSStatus {
    let f = &*(data as *const Arc<dyn Fn() + Send + Sync>);
    f();
    0
}

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        for sel in WATCHED {
            unsafe {
                AudioObjectRemovePropertyListener(kAudioObjectSystemObject, &AudioObjectPropertyAddress::global(sel), listener, self.data);
            }
        }
        // The box is leaked on purpose: a notification may still be running
        // on the HAL thread. It only holds a channel sender.
    }
}

impl Backend for MacBackend {
    fn snapshot(&self, levels: bool) -> Result<Snapshot> {
        let def_out = default_device_id(DeviceKind::Output);
        let def_in = default_device_id(DeviceKind::Input);
        let def_sys = default_device_id(DeviceKind::SystemOutput);
        let mut snap = Snapshot::default();
        for id in device_ids() {
            let Some(d) = read_device(id) else { continue };
            if id == def_out {
                snap.default_output = Some(d.uid.clone());
            }
            if id == def_in {
                snap.default_input = Some(d.uid.clone());
            }
            if id == def_sys {
                snap.system_output = Some(d.uid.clone());
            }
            if d.out_ch > 0 && (id == def_out || can_be_default(id, kAudioObjectPropertyScopeOutput, false)) {
                snap.outputs.push(AudioDevice {
                    uid: d.uid.clone(),
                    name: d.name.clone(),
                    transport: d.transport,
                    is_default: id == def_out,
                    is_default_system: id == def_sys,
                    channels: d.out_ch,
                    volume: if levels { volume(id, kAudioObjectPropertyScopeOutput, d.out_ch) } else { None },
                    muted: if levels { mute(id, kAudioObjectPropertyScopeOutput) } else { None },
                });
            }
            if d.in_ch > 0 && (id == def_in || can_be_default(id, kAudioObjectPropertyScopeInput, false)) {
                snap.inputs.push(AudioDevice {
                    uid: d.uid.clone(),
                    name: d.name.clone(),
                    transport: d.transport,
                    is_default: id == def_in,
                    is_default_system: false,
                    channels: d.in_ch,
                    volume: if levels { volume(id, kAudioObjectPropertyScopeInput, d.in_ch) } else { None },
                    muted: if levels { mute(id, kAudioObjectPropertyScopeInput) } else { None },
                });
            }
        }
        Ok(snap)
    }

    fn set_default(&self, kind: DeviceKind, uid: &str) -> Result<()> {
        let d = self.dev_for(uid)?;
        let (ch, system) = match kind {
            DeviceKind::Input => (d.in_ch, false),
            DeviceKind::Output => (d.out_ch, false),
            DeviceKind::SystemOutput => (d.out_ch, true),
        };
        if ch == 0 {
            return Err(anyhow!("{} has no {kind:?} channels", d.name));
        }
        if !can_be_default(d.id, scope_of(kind), system) {
            return Err(anyhow!("{} cannot be the default {kind:?} device", d.name));
        }
        if default_device_id(kind) == d.id {
            return Ok(());
        }
        props::set::<u32>(kAudioObjectSystemObject, AudioObjectPropertyAddress::global(default_selector(kind)), d.id)
    }

    fn set_volume(&self, uid: &str, v: f32) -> Result<()> {
        let d = self.dev_for(uid)?;
        let (scope, ch) =
            if d.out_ch > 0 { (kAudioObjectPropertyScopeOutput, d.out_ch) } else { (kAudioObjectPropertyScopeInput, d.in_ch) };
        set_volume(d.id, scope, ch, v)
    }

    fn set_mute(&self, uid: &str, m: bool) -> Result<()> {
        let d = self.dev_for(uid)?;
        let (scope, ch) =
            if d.out_ch > 0 { (kAudioObjectPropertyScopeOutput, d.out_ch) } else { (kAudioObjectPropertyScopeInput, d.in_ch) };
        set_mute(d.id, scope, ch, m)
    }

    fn watch(&self, notify: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn Send>> {
        let data = Box::into_raw(Box::new(notify)) as *mut c_void;
        for sel in WATCHED {
            let st = unsafe { AudioObjectAddPropertyListener(kAudioObjectSystemObject, &AudioObjectPropertyAddress::global(sel), listener, data) };
            if st != 0 && sel != kAudioHardwarePropertyProcessObjectList {
                log::warn!("audio-control: listening for {} failed: {}", status_str(sel as i32), status_str(st));
            }
        }
        Ok(Box::new(ListenerGuard { data }))
    }

    fn per_app_supported(&self) -> bool {
        tap::supported()
    }

    fn max_app_volume(&self) -> f32 {
        crate::MAX_APP_VOLUME
    }

    fn apps(&self, levels: &BTreeMap<String, AppLevel>) -> Result<Vec<AudioApp>> {
        Ok(self.taps.lock().unwrap().apps(levels))
    }

    fn apply_app_levels(&self, levels: &BTreeMap<String, AppLevel>, enabled: bool) -> Result<()> {
        self.taps.lock().unwrap().apply(levels, enabled)
    }

    fn capture_permission(&self) -> CapturePermission {
        tcc::preflight()
    }

    fn app_diagnostics(&self) -> Vec<String> {
        self.taps.lock().unwrap().diagnostics()
    }

    fn request_capture_permission(&self) -> Result<bool> {
        tcc::request()
    }
}

impl Drop for MacBackend {
    fn drop(&mut self) {
        if let Ok(mut t) = self.taps.lock() {
            let _ = t.apply(&BTreeMap::new(), false);
        }
    }
}
