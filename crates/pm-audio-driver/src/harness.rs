//! A fake Core Audio host and a scripted check of a driver through its
//! vtable — the same calls coreaudiod makes, in-process, without
//! installing anything.
//!
//! Used by the unit tests (an in-process [`DriverObject`]), by
//! `tests/load_bundle.rs` and by `examples/host_harness.rs` (a built
//! `PowerMac Audio.driver` loaded through CFPlugIn).
//!
//! [`DriverObject`]: crate::driver::DriverObject

#![allow(non_upper_case_globals)]

use crate::spec::{self, DriverConfig, CONFIG_PROPERTY, MAIN_NAME, RING_FRAMES};
use crate::sys::*;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFGetTypeID, CFRelease};
use core_foundation_sys::string::{CFStringGetTypeID, CFStringRef};
use core_foundation_sys::uuid::CFUUIDGetConstantUUIDWithBytes;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

#[derive(Default, Debug)]
pub struct FakeHostState {
    /// (object, selectors) for every PropertiesChanged call.
    pub changes: Vec<(AudioObjectID, Vec<u32>)>,
    /// Host storage (string values only, which is all the driver stores).
    pub storage: HashMap<String, String>,
    /// Devices waiting for PerformDeviceConfigurationChange.
    pub requests: Vec<AudioObjectID>,
}

/// `AudioServerPlugInHostInterface` first, so the host ref is a pointer to
/// this.
#[repr(C)]
pub struct FakeHost {
    iface: AudioServerPlugInHostInterface,
    pub state: Mutex<FakeHostState>,
}

fn cf_to_string(r: CFStringRef) -> Option<String> {
    if r.is_null() || unsafe { CFGetTypeID(r as *const c_void) != CFStringGetTypeID() } {
        return None;
    }
    Some(unsafe { CFString::wrap_under_get_rule(r) }.to_string())
}

unsafe fn host<'a>(h: AudioServerPlugInHostRef) -> &'a FakeHost {
    &*(h as *const FakeHost)
}

unsafe extern "C" fn properties_changed(h: AudioServerPlugInHostRef, obj: AudioObjectID, n: u32, addrs: *const AudioObjectPropertyAddress) -> OSStatus {
    let sels = if addrs.is_null() { Vec::new() } else { std::slice::from_raw_parts(addrs, n as usize).iter().map(|a| a.mSelector).collect() };
    host(h).state.lock().unwrap().changes.push((obj, sels));
    0
}

unsafe extern "C" fn copy_from_storage(h: AudioServerPlugInHostRef, key: CFStringRef, out: *mut CFPropertyListRef) -> OSStatus {
    let Some(k) = cf_to_string(key) else { return kAudioHardwareIllegalOperationError };
    *out = match host(h).state.lock().unwrap().storage.get(&k) {
        Some(v) => {
            let s = CFString::new(v);
            let r = s.as_concrete_TypeRef();
            std::mem::forget(s);
            r as CFPropertyListRef
        }
        None => std::ptr::null(),
    };
    0
}

unsafe extern "C" fn write_to_storage(h: AudioServerPlugInHostRef, key: CFStringRef, data: CFPropertyListRef) -> OSStatus {
    let (Some(k), Some(v)) = (cf_to_string(key), cf_to_string(data as CFStringRef)) else { return kAudioHardwareIllegalOperationError };
    host(h).state.lock().unwrap().storage.insert(k, v);
    0
}

unsafe extern "C" fn delete_from_storage(h: AudioServerPlugInHostRef, key: CFStringRef) -> OSStatus {
    if let Some(k) = cf_to_string(key) {
        host(h).state.lock().unwrap().storage.remove(&k);
    }
    0
}

unsafe extern "C" fn request_config_change(h: AudioServerPlugInHostRef, dev: AudioObjectID, _: u64, _: *mut c_void) -> OSStatus {
    host(h).state.lock().unwrap().requests.push(dev);
    0
}

impl FakeHost {
    pub fn new() -> Box<FakeHost> {
        Box::new(FakeHost {
            iface: AudioServerPlugInHostInterface {
                PropertiesChanged: properties_changed,
                CopyFromStorage: copy_from_storage,
                WriteToStorage: write_to_storage,
                DeleteFromStorage: delete_from_storage,
                RequestDeviceConfigurationChange: request_config_change,
            },
            state: Mutex::new(FakeHostState::default()),
        })
    }

    pub fn host_ref(&self) -> AudioServerPlugInHostRef {
        &self.iface as *const AudioServerPlugInHostInterface
    }
}

/// Typed calls through a driver's vtable.
pub struct DriverClient {
    drv: AudioServerPlugInDriverRef,
    vt: &'static AudioServerPlugInDriverInterface,
}

pub type Result<T> = std::result::Result<T, String>;

fn st(what: &str, s: OSStatus) -> Result<()> {
    if s == 0 {
        Ok(())
    } else {
        let b = (s as u32).to_be_bytes();
        let cc = if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') { format!("'{}'", String::from_utf8_lossy(&b)) } else { s.to_string() };
        Err(format!("{what} failed: {cc}"))
    }
}

pub fn global(sel: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress::global(sel)
}

impl DriverClient {
    /// # Safety
    /// `drv` must be an `AudioServerPlugInDriverRef` that outlives the
    /// client.
    pub unsafe fn new(drv: AudioServerPlugInDriverRef) -> DriverClient {
        let vt = &**(drv as *const *const AudioServerPlugInDriverInterface);
        DriverClient { drv, vt }
    }

    pub fn query_interface(&self, uuid: [u8; 16]) -> Option<*mut c_void> {
        let mut out = std::ptr::null_mut();
        let hr = unsafe { (self.vt.QueryInterface)(self.drv, to_uuid_bytes(uuid), &mut out) };
        (hr == S_OK && !out.is_null()).then_some(out)
    }

    pub fn release(&self) -> u32 {
        unsafe { (self.vt.Release)(self.drv) }
    }

    pub fn initialize(&self, host: &FakeHost) -> Result<()> {
        st("Initialize", unsafe { (self.vt.Initialize)(self.drv, host.host_ref()) })
    }

    pub fn has(&self, obj: AudioObjectID, addr: AudioObjectPropertyAddress) -> bool {
        unsafe { (self.vt.HasProperty)(self.drv, obj, 0, &addr) != 0 }
    }

    pub fn settable(&self, obj: AudioObjectID, addr: AudioObjectPropertyAddress) -> Result<bool> {
        let mut s = 0u8;
        st("IsPropertySettable", unsafe { (self.vt.IsPropertySettable)(self.drv, obj, 0, &addr, &mut s) })?;
        Ok(s != 0)
    }

    /// Raw bytes (8-byte aligned storage) of a property.
    pub fn get_raw(&self, obj: AudioObjectID, addr: AudioObjectPropertyAddress, qualifier: &[u8]) -> Result<Vec<u8>> {
        let (qs, qp) = (qualifier.len() as u32, if qualifier.is_empty() { std::ptr::null() } else { qualifier.as_ptr() as *const c_void });
        let mut size = 0u32;
        st("GetPropertyDataSize", unsafe { (self.vt.GetPropertyDataSize)(self.drv, obj, 0, &addr, qs, qp, &mut size) })?;
        let mut buf = vec![0u64; (size as usize).div_ceil(8).max(1)];
        let mut out = 0u32;
        st("GetPropertyData", unsafe {
            (self.vt.GetPropertyData)(self.drv, obj, 0, &addr, qs, qp, size, &mut out, buf.as_mut_ptr() as *mut c_void)
        })?;
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, out as usize) };
        Ok(bytes.to_vec())
    }

    pub fn get_u32(&self, obj: AudioObjectID, addr: AudioObjectPropertyAddress) -> Result<u32> {
        let b = self.get_raw(obj, addr, &[])?;
        b.get(..4).map(|b| u32::from_ne_bytes(b.try_into().unwrap())).ok_or_else(|| "short u32".into())
    }

    pub fn get_f32(&self, obj: AudioObjectID, addr: AudioObjectPropertyAddress) -> Result<f32> {
        self.get_u32(obj, addr).map(f32::from_bits)
    }

    pub fn get_f64(&self, obj: AudioObjectID, addr: AudioObjectPropertyAddress) -> Result<f64> {
        let b = self.get_raw(obj, addr, &[])?;
        b.get(..8).map(|b| f64::from_ne_bytes(b.try_into().unwrap())).ok_or_else(|| "short f64".into())
    }

    pub fn get_ids(&self, obj: AudioObjectID, addr: AudioObjectPropertyAddress, qualifier: &[u8]) -> Result<Vec<u32>> {
        Ok(self.get_raw(obj, addr, qualifier)?.as_chunks::<4>().0.iter().map(|c| u32::from_ne_bytes(*c)).collect())
    }

    pub fn get_string(&self, obj: AudioObjectID, addr: AudioObjectPropertyAddress) -> Result<String> {
        let b = self.get_raw(obj, addr, &[])?;
        let r = usize::from_ne_bytes(b.get(..8).ok_or("short CFString")?.try_into().unwrap()) as CFStringRef;
        let s = cf_to_string(r).ok_or("not a CFString")?;
        unsafe { CFRelease(r as *const c_void) };
        Ok(s)
    }

    pub fn get_format(&self, stream: AudioObjectID, sel: u32) -> Result<AudioStreamBasicDescription> {
        let b = self.get_raw(stream, global(sel), &[])?;
        if b.len() < std::mem::size_of::<AudioStreamBasicDescription>() {
            return Err("short ASBD".into());
        }
        Ok(unsafe { (b.as_ptr() as *const AudioStreamBasicDescription).read_unaligned() })
    }

    pub fn set_raw(&self, obj: AudioObjectID, addr: AudioObjectPropertyAddress, data: &[u8]) -> Result<()> {
        st("SetPropertyData", unsafe {
            (self.vt.SetPropertyData)(self.drv, obj, 0, &addr, 0, std::ptr::null(), data.len() as u32, data.as_ptr() as *const c_void)
        })
    }

    pub fn set_string(&self, obj: AudioObjectID, addr: AudioObjectPropertyAddress, s: &str) -> Result<()> {
        let cf = CFString::new(s);
        let r = cf.as_concrete_TypeRef() as usize;
        self.set_raw(obj, addr, &r.to_ne_bytes())
    }

    /// Answer the host's queued configuration-change requests.
    pub fn pump(&self, host: &FakeHost) -> Result<usize> {
        let reqs: Vec<_> = std::mem::take(&mut host.state.lock().unwrap().requests);
        for &dev in &reqs {
            st("PerformDeviceConfigurationChange", unsafe {
                (self.vt.PerformDeviceConfigurationChange)(self.drv, dev, 0, std::ptr::null_mut())
            })?;
        }
        Ok(reqs.len())
    }

    pub fn start_io(&self, dev: AudioObjectID) -> Result<()> {
        st("StartIO", unsafe { (self.vt.StartIO)(self.drv, dev, 1) })
    }

    pub fn stop_io(&self, dev: AudioObjectID) -> Result<()> {
        st("StopIO", unsafe { (self.vt.StopIO)(self.drv, dev, 1) })
    }

    pub fn zero_timestamp(&self, dev: AudioObjectID) -> Result<(f64, u64, u64)> {
        let (mut s, mut h, mut seed) = (0.0, 0u64, 0u64);
        st("GetZeroTimeStamp", unsafe { (self.vt.GetZeroTimeStamp)(self.drv, dev, 1, &mut s, &mut h, &mut seed) })?;
        Ok((s, h, seed))
    }

    pub fn will_do(&self, dev: AudioObjectID, op: u32) -> Result<(bool, bool)> {
        let (mut w, mut ip) = (0u8, 0u8);
        st("WillDoIOOperation", unsafe { (self.vt.WillDoIOOperation)(self.drv, dev, 1, op, &mut w, &mut ip) })?;
        Ok((w != 0, ip != 0))
    }

    /// One IO operation the way the host runs it (Begin, Do, End).
    pub fn io(&self, dev: AudioObjectID, stream: AudioObjectID, op: u32, cycle: &AudioServerPlugInIOCycleInfo, buf: &mut [f32], channels: u32) -> Result<()> {
        let frames = (buf.len() / channels as usize) as u32;
        unsafe {
            st("BeginIOOperation", (self.vt.BeginIOOperation)(self.drv, dev, 1, op, frames, cycle))?;
            st(
                "DoIOOperation",
                (self.vt.DoIOOperation)(self.drv, dev, stream, 1, op, frames, cycle, buf.as_mut_ptr() as *mut c_void, std::ptr::null_mut()),
            )?;
            st("EndIOOperation", (self.vt.EndIOOperation)(self.drv, dev, 1, op, frames, cycle))
        }
    }
}

/// `kAudioServerPlugInTypeUUID` as a CFUUIDRef.
pub fn plugin_type_uuid() -> core_foundation_sys::uuid::CFUUIDRef {
    let b = PLUGIN_TYPE_UUID_BYTES;
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            std::ptr::null(),
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
        )
    }
}

fn ensure(cond: bool, msg: impl Into<String>) -> Result<()> {
    if cond {
        Ok(())
    } else {
        Err(msg.into())
    }
}

/// Describe one device; returns (input stream, output stream, channels).
fn check_device(c: &DriverClient, dev: AudioObjectID, log: &mut Vec<String>) -> Result<(AudioObjectID, AudioObjectID, u32)> {
    ensure(c.get_u32(dev, global(kAudioObjectPropertyClass))? == kAudioDeviceClassID, "device class")?;
    let name = c.get_string(dev, global(kAudioObjectPropertyName))?;
    let uid = c.get_string(dev, global(kAudioDevicePropertyDeviceUID))?;
    let rate = c.get_f64(dev, global(kAudioDevicePropertyNominalSampleRate))?;
    let ins = c.get_ids(dev, AudioObjectPropertyAddress::new(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput, 0), &[])?;
    let outs = c.get_ids(dev, AudioObjectPropertyAddress::new(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput, 0), &[])?;
    ensure(ins.len() == 1 && outs.len() == 1, format!("{name}: expected one input and one output stream"))?;
    let fmt = c.get_format(outs[0], kAudioStreamPropertyVirtualFormat)?;
    let in_fmt = c.get_format(ins[0], kAudioStreamPropertyPhysicalFormat)?;
    ensure(fmt == in_fmt, "input and output formats differ")?;
    ensure(fmt.mSampleRate == rate, "stream rate != device rate")?;
    ensure(fmt.mFormatID == kAudioFormatLinearPCM && fmt.mBitsPerChannel == 32, "not float32 PCM")?;
    ensure(c.get_u32(dev, global(kAudioDevicePropertyZeroTimeStampPeriod))? == RING_FRAMES, "period")?;
    ensure(c.get_u32(dev, global(kAudioDevicePropertyLatency))? == 0, "latency")?;
    ensure(c.get_u32(dev, global(kAudioDevicePropertyTransportType))? == kAudioDeviceTransportTypeVirtual, "transport")?;
    ensure(c.has(dev, global(kAudioDevicePropertyPreferredChannelLayout)), "channel layout")?;
    ensure(c.settable(dev, global(kAudioDevicePropertyNominalSampleRate))?, "rate settable")?;
    let controls = c.get_ids(dev, global(kAudioObjectPropertyControlList), &[])?;
    ensure(controls.len() == 2, "volume + mute controls")?;
    log.push(format!("device {dev} \"{name}\" ({uid}): {} ch, {rate} Hz", fmt.mChannelsPerFrame));
    Ok((ins[0], outs[0], fmt.mChannelsPerFrame))
}

/// Run a device through a loopback: write a test signal with WriteMix at
/// output time `t`, read it back with ReadInput at input time `t`.
fn check_loopback(c: &DriverClient, dev: AudioObjectID, input: AudioObjectID, output: AudioObjectID, ch: u32, log: &mut Vec<String>) -> Result<()> {
    ensure(c.will_do(dev, kAudioServerPlugInIOOperationWriteMix)? == (true, true), "WillDo WriteMix")?;
    ensure(c.will_do(dev, kAudioServerPlugInIOOperationReadInput)? == (true, true), "WillDo ReadInput")?;
    ensure(!c.will_do(dev, u32::from_be_bytes(*b"pmix"))?.0, "WillDo ProcessMix should be false")?;
    c.start_io(dev)?;
    let r: Result<()> = (|| {
        ensure(c.get_u32(dev, global(kAudioDevicePropertyDeviceIsRunning))? == 1, "running")?;
        let (s0, h0, seed) = c.zero_timestamp(dev)?;
        ensure(s0 == 0.0 && seed != 0, "first zero timestamp")?;
        let frames = 512usize;
        let mut cycle = AudioServerPlugInIOCycleInfo { mNominalIOBufferFrameSize: frames as u32, ..Default::default() };
        let mut sent = Vec::new();
        let mut got = Vec::new();
        for n in 0..8u64 {
            // Output runs two buffers ahead of input, as in the HAL.
            cycle.mIOCycleCounter = n;
            cycle.mOutputTime.mSampleTime = ((n + 2) * frames as u64) as f64;
            cycle.mInputTime.mSampleTime = (n * frames as u64) as f64;
            let mut out: Vec<f32> = (0..frames * ch as usize)
                .map(|i| {
                    let t = ((n + 2) as usize * frames + i / ch as usize) as f32;
                    (t * 0.05 + (i % ch as usize) as f32).sin() * 0.5
                })
                .collect();
            c.io(dev, output, kAudioServerPlugInIOOperationWriteMix, &cycle, &mut out, ch)?;
            sent.push(out);
            let mut inp = vec![1.0f32; frames * ch as usize];
            c.io(dev, input, kAudioServerPlugInIOOperationReadInput, &cycle, &mut inp, ch)?;
            got.push(inp);
        }
        ensure(got[0].iter().chain(&got[1]).all(|&s| s == 0.0), "input before any output must be silent")?;
        for n in 2..8 {
            ensure(got[n] == sent[n - 2], format!("loopback mismatch in cycle {n}"))?;
        }
        // Volume 50 % → gain 0.25.
        let vol = c.get_ids(dev, AudioObjectPropertyAddress::new(kAudioObjectPropertyOwnedObjects, kAudioObjectPropertyScopeOutput, 0), &kAudioVolumeControlClassID.to_ne_bytes())?;
        ensure(vol.len() == 1, "one volume control")?;
        c.set_raw(vol[0], global(kAudioLevelControlPropertyScalarValue), &0.5f32.to_ne_bytes())?;
        cycle.mOutputTime.mSampleTime = 100_000.0;
        cycle.mInputTime.mSampleTime = 100_000.0;
        let mut out = vec![0.8f32; frames * ch as usize];
        c.io(dev, output, kAudioServerPlugInIOOperationWriteMix, &cycle, &mut out, ch)?;
        let mut inp = vec![0.0f32; frames * ch as usize];
        c.io(dev, input, kAudioServerPlugInIOOperationReadInput, &cycle, &mut inp, ch)?;
        ensure(inp.iter().all(|&s| (s - 0.2).abs() < 1e-6), "volume scaling")?;
        c.set_raw(vol[0], global(kAudioLevelControlPropertyScalarValue), &1.0f32.to_ne_bytes())?;
        let (_, h1, _) = c.zero_timestamp(dev)?;
        ensure(h1 >= h0, "host time went backwards")?;
        log.push(format!("loopback ok on device {dev}: 6 cycles × {frames} frames × {ch} ch bit-exact, volume applied"));
        Ok(())
    })();
    c.stop_io(dev)?;
    r?;
    ensure(c.get_u32(dev, global(kAudioDevicePropertyDeviceIsRunning))? == 0, "stopped")?;
    Ok(())
}

/// Drive a driver the way the host does and check the results. Returns a
/// log of what was checked. Changes the driver's configuration (in this
/// process only).
pub fn exercise(c: &DriverClient, host: &FakeHost) -> Result<Vec<String>> {
    let mut log = Vec::new();
    let unk = c.query_interface(IUNKNOWN_UUID_BYTES).ok_or("QueryInterface(IUnknown)")?;
    let iface = c.query_interface(DRIVER_INTERFACE_UUID_BYTES).ok_or("QueryInterface(driver interface)")?;
    ensure(unk == iface, "IUnknown and driver interface differ")?;
    ensure(c.query_interface([7; 16]).is_none(), "unknown interface accepted")?;
    c.release();
    c.release();
    c.initialize(host)?;
    log.push("QueryInterface + Initialize ok".into());

    let p = kAudioObjectPlugInObject;
    ensure(c.get_u32(p, global(kAudioObjectPropertyClass))? == kAudioPlugInClassID, "plug-in class")?;
    ensure(c.get_string(p, global(kAudioPlugInPropertyBundleID))? == spec::BUNDLE_ID, "bundle id")?;
    let devs = c.get_ids(p, global(kAudioPlugInPropertyDeviceList), &[])?;
    ensure(!devs.is_empty(), "no devices")?;
    let main_name = c.get_string(devs[0], global(kAudioObjectPropertyName))?;
    log.push(format!("{} device(s); first is \"{main_name}\"", devs.len()));
    for &d in &devs {
        check_device(c, d, &mut log)?;
    }
    let cfg_json = c.get_string(p, global(CONFIG_PROPERTY))?;
    DriverConfig::from_json(&cfg_json)?;

    // Reconfigure through the custom property: main + two mixes, 8 ch.
    let want = DriverConfig::with_mixes(8, 48_000.0, 2);
    ensure(c.settable(p, global(CONFIG_PROPERTY))?, "config settable")?;
    c.set_string(p, global(CONFIG_PROPERTY), &want.to_json())?;
    let n = c.pump(host)?;
    let devs = c.get_ids(p, global(kAudioPlugInPropertyDeviceList), &[])?;
    ensure(devs.len() == 3, format!("expected 3 devices after reconfiguring, got {}", devs.len()))?;
    {
        let hs = host.state.lock().unwrap();
        ensure(hs.changes.iter().any(|(o, s)| *o == p && s.contains(&kAudioPlugInPropertyDeviceList)), "no DeviceList notification")?;
        let stored = hs.storage.get(spec::STORAGE_KEY).ok_or("config not stored")?;
        ensure(DriverConfig::from_json(stored)? == want, "stored config differs")?;
    }
    let names: Vec<String> = devs.iter().map(|&d| c.get_string(d, global(kAudioObjectPropertyName))).collect::<Result<_>>()?;
    ensure(names == ["PowerMac Audio", "PowerMac Mix 1", "PowerMac Mix 2"], format!("names {names:?}"))?;
    log.push(format!("reconfigured to {names:?} ({n} config change(s) performed)"));
    let bad = c.set_string(p, global(CONFIG_PROPERTY), r#"{"devices":[{"uid":"a","name":"A","channels":3}]}"#);
    ensure(bad.is_err(), "invalid config accepted")?;

    let mut streams = Vec::new();
    for &d in &devs {
        streams.push(check_device(c, d, &mut log)?);
    }

    // Sample-rate change through the host's configuration-change protocol.
    let dev = devs[0];
    c.set_raw(dev, global(kAudioDevicePropertyNominalSampleRate), &96_000f64.to_ne_bytes())?;
    ensure(c.get_f64(dev, global(kAudioDevicePropertyNominalSampleRate))? == 48_000.0, "rate changed before Perform")?;
    ensure(c.pump(host)? == 1, "no configuration change requested")?;
    ensure(c.get_f64(dev, global(kAudioDevicePropertyNominalSampleRate))? == 96_000.0, "rate not applied")?;
    let fmt = c.get_format(streams[0].1, kAudioStreamPropertyVirtualFormat)?;
    ensure(fmt.mSampleRate == 96_000.0, "stream rate not applied")?;
    log.push("NominalSampleRate 48000 → 96000 via RequestDeviceConfigurationChange ok".into());

    for (i, &d) in devs.iter().enumerate() {
        let (input, output, ch) = streams[i];
        check_loopback(c, d, input, output, ch, &mut log)?;
    }

    // Back to the default so a re-run starts from the same place.
    c.set_string(p, global(CONFIG_PROPERTY), &DriverConfig::default().to_json())?;
    c.pump(host)?;
    let devs = c.get_ids(p, global(kAudioPlugInPropertyDeviceList), &[])?;
    ensure(devs.len() == 1 && c.get_string(devs[0], global(kAudioObjectPropertyName))? == MAIN_NAME, "restore default")?;
    log.push("restored the default configuration".into());
    Ok(log)
}

// ---- loading a built bundle ------------------------------------------------------

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFPlugInCreate(alloc: *const c_void, url: core_foundation_sys::url::CFURLRef) -> *mut c_void;
    fn CFPlugInFindFactoriesForPlugInType(type_uuid: core_foundation_sys::uuid::CFUUIDRef) -> core_foundation_sys::array::CFArrayRef;
    fn CFPlugInInstanceCreate(
        alloc: *const c_void,
        factory: core_foundation_sys::uuid::CFUUIDRef,
        type_uuid: core_foundation_sys::uuid::CFUUIDRef,
    ) -> *mut c_void;
}

/// Load a `.driver` bundle the way coreaudiod does — CFPlugIn reads
/// `CFPlugInFactories`/`CFPlugInTypes` from its Info.plist, loads the
/// executable and calls the factory for `kAudioServerPlugInTypeUUID` — and
/// return the driver interface. The bundle stays loaded for the life of the
/// process.
pub fn load_bundle(path: &std::path::Path) -> Result<AudioServerPlugInDriverRef> {
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex};
    use core_foundation_sys::url::CFURLCreateFromFileSystemRepresentation;
    use std::os::unix::ffi::OsStrExt;
    let p = path.as_os_str().as_bytes();
    unsafe {
        let url = CFURLCreateFromFileSystemRepresentation(std::ptr::null(), p.as_ptr(), p.len() as _, 1);
        if url.is_null() {
            return Err("bad bundle path".into());
        }
        let plugin = CFPlugInCreate(std::ptr::null(), url);
        CFRelease(url as *const c_void);
        if plugin.is_null() {
            return Err(format!("CFPlugInCreate failed for {}", path.display()));
        }
        let factories = CFPlugInFindFactoriesForPlugInType(plugin_type_uuid());
        if factories.is_null() || CFArrayGetCount(factories) == 0 {
            return Err("no factory for kAudioServerPlugInTypeUUID (check CFPlugInTypes/CFPlugInFactories)".into());
        }
        let factory = CFArrayGetValueAtIndex(factories, 0) as core_foundation_sys::uuid::CFUUIDRef;
        let unknown = CFPlugInInstanceCreate(std::ptr::null(), factory, plugin_type_uuid());
        CFRelease(factories as *const c_void);
        if unknown.is_null() {
            return Err("CFPlugInInstanceCreate returned null (executable not loaded or factory missing)".into());
        }
        let c = DriverClient::new(unknown);
        let drv = c.query_interface(DRIVER_INTERFACE_UUID_BYTES).ok_or("the instance has no AudioServerPlugInDriverInterface")?;
        c.release();
        Ok(drv)
    }
}

/// Assemble `<dir>/PowerMac Audio.driver` from a built dylib (what
/// scripts/build-audio-driver.sh does, minus signing; the linker's ad-hoc
/// signature is enough to load it in-process).
pub fn assemble_bundle(dylib: &std::path::Path, dir: &std::path::Path, version: &str) -> Result<std::path::PathBuf> {
    let bundle = dir.join(spec::BUNDLE_NAME);
    let _ = std::fs::remove_dir_all(&bundle);
    let macos = bundle.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).map_err(|e| e.to_string())?;
    std::fs::copy(dylib, macos.join(spec::EXECUTABLE_NAME)).map_err(|e| e.to_string())?;
    std::fs::write(bundle.join("Contents/Info.plist"), spec::info_plist(version)).map_err(|e| e.to_string())?;
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::DriverObject;

    #[test]
    fn exercise_in_process_driver() {
        let obj = DriverObject::new();
        let host = FakeHost::new();
        let c = unsafe { DriverClient::new(obj.as_ref_ptr()) };
        let log = exercise(&c, &host).unwrap_or_else(|e| panic!("{e}"));
        assert!(log.iter().any(|l| l.starts_with("loopback ok")), "{log:?}");
    }

    #[test]
    fn initialize_reads_stored_config() {
        let obj = DriverObject::new();
        let host = FakeHost::new();
        let cfg = DriverConfig::with_mixes(32, 44_100.0, 4);
        host.state.lock().unwrap().storage.insert(spec::STORAGE_KEY.into(), cfg.to_json());
        let c = unsafe { DriverClient::new(obj.as_ref_ptr()) };
        c.initialize(&host).unwrap();
        let devs = c.get_ids(1, global(kAudioPlugInPropertyDeviceList), &[]).unwrap();
        assert_eq!(devs.len(), 5);
        assert_eq!(c.get_string(devs[4], global(kAudioObjectPropertyName)).unwrap(), "PowerMac Mix 4");
        assert_eq!(c.get_f64(devs[0], global(kAudioDevicePropertyNominalSampleRate)).unwrap(), 44_100.0);
        let outs = c.get_ids(devs[0], AudioObjectPropertyAddress::new(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput, 0), &[]).unwrap();
        assert_eq!(c.get_format(outs[0], kAudioStreamPropertyVirtualFormat).unwrap().mChannelsPerFrame, 32);
        // Nothing is written back during Initialize.
        assert!(host.state.lock().unwrap().changes.is_empty());
    }

    #[test]
    fn corrupt_stored_config_falls_back_to_default() {
        let obj = DriverObject::new();
        let host = FakeHost::new();
        host.state.lock().unwrap().storage.insert(spec::STORAGE_KEY.into(), "{not json".into());
        let c = unsafe { DriverClient::new(obj.as_ref_ptr()) };
        c.initialize(&host).unwrap();
        let devs = c.get_ids(1, global(kAudioPlugInPropertyDeviceList), &[]).unwrap();
        assert_eq!(devs.len(), 1);
        assert_eq!(c.get_string(devs[0], global(kAudioObjectPropertyName)).unwrap(), MAIN_NAME);
    }
}
