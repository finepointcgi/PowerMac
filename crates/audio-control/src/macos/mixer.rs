//! macOS mixer device: process taps + one private aggregate device + an
//! IOProc running the mixer graph. See `crate::mixer` for the design.

use super::ffi::*;
use super::tap::{self, cfs, create_tap, destroy_tap_object, is_float32, CA_TAP_MUTED_WHEN_TAPPED, CA_TAP_UNMUTED};
use super::{device_ids, device_uid, id_for_uid, props, tcc, AGGREGATE_UID_PREFIX};
use crate::mixer::graph::{Graph, IoBuffers};
use crate::mixer::{InvApp, InvDevice, Inventory, MixDevice, Realized, RealizedSub, Topology};
use anyhow::{anyhow, Result};
use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use objc2::rc::autoreleasepool;
use objc2_foundation::NSUUID;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Channels of each stream of `id` in `scope`.
pub(crate) fn stream_channels(id: AudioObjectID, scope: u32) -> Vec<u32> {
    let addr = AudioObjectPropertyAddress::new(kAudioDevicePropertyStreamConfiguration, scope, kAudioObjectPropertyElementMain);
    let Ok(buf) = props::get_bytes(id, addr) else { return Vec::new() };
    let abl = buf.as_ptr() as *const AudioBufferList;
    let max = (buf.len() * 8).saturating_sub(8) / std::mem::size_of::<AudioBuffer>();
    unsafe {
        let n = ((*abl).mNumberBuffers as usize).min(max);
        let first = std::ptr::addr_of!((*abl).mBuffers) as *const AudioBuffer;
        (0..n).map(|i| (*first.add(i)).mNumberChannels).collect()
    }
}

fn nominal_rate(id: AudioObjectID) -> f64 {
    props::get::<f64>(id, AudioObjectPropertyAddress::global(kAudioDevicePropertyNominalSampleRate)).unwrap_or(0.0)
}

/// Read-only: devices with stream layouts, apps with process objects.
pub(crate) fn inventory(taps: &mut tap::TapManager) -> Inventory {
    let mut devices = Vec::new();
    for id in device_ids() {
        let Some(uid) = device_uid(id) else { continue };
        if uid.starts_with(AGGREGATE_UID_PREFIX) {
            continue;
        }
        if props::get::<u32>(id, AudioObjectPropertyAddress::global(kAudioDevicePropertyIsHidden)).unwrap_or(0) != 0 {
            continue;
        }
        let transport = super::transport_of(props::get::<u32>(id, AudioObjectPropertyAddress::global(kAudioDevicePropertyTransportType)).unwrap_or(0));
        devices.push(InvDevice {
            name: props::get_string(id, AudioObjectPropertyAddress::global(kAudioObjectPropertyName)).unwrap_or_default(),
            transport,
            input_streams: stream_channels(id, kAudioObjectPropertyScopeInput),
            output_streams: stream_channels(id, kAudioObjectPropertyScopeOutput),
            sample_rate: nominal_rate(id),
            uid,
        });
    }
    let mut apps: BTreeMap<String, InvApp> = BTreeMap::new();
    for p in taps.processes() {
        let a = apps.entry(p.app_id.clone()).or_insert_with(|| InvApp { bundle_id: p.app_id.clone(), name: p.name.clone(), ..Default::default() });
        a.objects.push(p.obj);
        a.pids.push(p.pid);
    }
    Inventory { devices, apps: apps.into_values().collect(), taps_supported: tap::supported(), capture_permission: tcc::preflight() }
}

/// Shared with the IOProc.
struct IoCtx {
    graph: Box<Graph>,
    poisoned: AtomicBool,
}

pub(crate) struct MacMixDevice {
    aggregate: AudioObjectID,
    taps: Vec<AudioObjectID>,
    realized: Realized,
    /// Stream on/off per aggregate stream (input, output) for our IOProc.
    usage: (Vec<bool>, Vec<bool>),
    proc_id: AudioDeviceIOProcID,
    ctx: *mut IoCtx,
    name: String,
}

// The raw pointers are owned by this value and shared only with our IOProc.
unsafe impl Send for MacMixDevice {}

fn sub_dict(uid: &str, drift: bool) -> CFDictionary<core_foundation::base::CFType, core_foundation::base::CFType> {
    CFDictionary::from_CFType_pairs(&[(cfs("uid"), cfs(uid)), (cfs("drift"), CFNumber::from(drift as i32).as_CFType())])
}

impl MacMixDevice {
    pub(crate) fn open(topo: &Topology) -> Result<MacMixDevice> {
        if topo.subdevices.is_empty() || topo.subdevices[0].uid != topo.clock {
            return Err(anyhow!("the clock device must be the first sub-device"));
        }
        let mut dev = MacMixDevice {
            aggregate: 0,
            taps: Vec::new(),
            realized: Realized::default(),
            usage: (Vec::new(), Vec::new()),
            proc_id: std::ptr::null_mut(),
            ctx: std::ptr::null_mut(),
            name: String::new(),
        };
        // On any error below, Drop destroys whatever was created.
        let mut tap_uids = Vec::new();
        for t in &topo.taps {
            if t.objects.is_empty() {
                return Err(anyhow!("{} has no audio processes", t.bundle_id));
            }
            let behavior = if t.mute_original { CA_TAP_MUTED_WHEN_TAPPED } else { CA_TAP_UNMUTED };
            let id = create_tap(&t.objects, behavior).map_err(|e| anyhow!("tapping {}: {e:#}", t.bundle_id))?;
            dev.taps.push(id);
            tap_uids.push(props::get_string(id, AudioObjectPropertyAddress::global(kAudioTapPropertyUID))?);
        }
        for s in &topo.subdevices {
            if id_for_uid(&s.uid).is_none() {
                return Err(anyhow!("{} is not connected", s.uid));
            }
        }

        let uid = format!("{AGGREGATE_UID_PREFIX}mixer.{}", autoreleasepool(|_| NSUUID::new().UUIDString().to_string()));
        let subs: Vec<_> = topo.subdevices.iter().map(|s| sub_dict(&s.uid, s.uid != topo.clock).as_CFType()).collect();
        let taps: Vec<_> = tap_uids.iter().map(|u| sub_dict(u, true).as_CFType()).collect();
        let mut pairs = vec![
            (cfs("uid"), cfs(&uid)),
            (cfs("name"), cfs("PowerMac Mixer")),
            (cfs("master"), cfs(&topo.clock)),
            (cfs("clock"), cfs(&topo.clock)),
            (cfs("private"), CFNumber::from(1i32).as_CFType()),
            (cfs("stacked"), CFNumber::from(0i32).as_CFType()),
            (cfs("subdevices"), CFArray::from_CFTypes(&subs).as_CFType()),
        ];
        if !taps.is_empty() {
            pairs.push((cfs("tapautostart"), CFNumber::from(1i32).as_CFType()));
            pairs.push((cfs("taps"), CFArray::from_CFTypes(&taps).as_CFType()));
        }
        let desc = CFDictionary::from_CFType_pairs(&pairs);
        let mut agg: AudioObjectID = 0;
        check(unsafe { AudioHardwareCreateAggregateDevice(desc.as_concrete_TypeRef(), &mut agg) }, "AudioHardwareCreateAggregateDevice")?;
        dev.aggregate = agg;
        dev.name = uid;

        // The HAL may attach sub-devices and taps a moment later: wait until
        // the aggregate's streams match what its parts say.
        let deadline = Instant::now() + Duration::from_millis(1500);
        let (realized, ins, outs) = loop {
            let r = dev.read_realized(topo);
            let ins = stream_channels(agg, kAudioObjectPropertyScopeInput);
            let outs = stream_channels(agg, kAudioObjectPropertyScopeOutput);
            let complete = r.subdevices.len() == topo.subdevices.len() && r.taps.len() == topo.taps.len();
            if (complete && ins == r.input_streams() && outs == r.output_streams()) || Instant::now() > deadline {
                break (r, ins, outs);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if ins != realized.input_streams() || outs != realized.output_streams() {
            return Err(anyhow!(
                "the mixer device's streams don't match its parts (inputs {ins:?} vs {:?}, outputs {outs:?} vs {:?})",
                realized.input_streams(),
                realized.output_streams()
            ));
        }
        if realized.subdevices.len() != topo.subdevices.len() || realized.taps.len() != topo.taps.len() {
            return Err(anyhow!(
                "the mixer device has {} of {} devices and {} of {} taps",
                realized.subdevices.len(),
                topo.subdevices.len(),
                realized.taps.len(),
                topo.taps.len()
            ));
        }
        if (!outs.is_empty() && !is_float32(agg, kAudioObjectPropertyScopeOutput)) || (!ins.is_empty() && !is_float32(agg, kAudioObjectPropertyScopeInput)) {
            return Err(anyhow!("a device of the mixer does not use 32-bit float streams"));
        }
        // Which streams our IOProc needs.
        let mut in_on = Vec::new();
        let mut out_on = Vec::new();
        for s in &realized.subdevices {
            let want = topo.subdevices.iter().find(|t| t.uid == s.uid);
            in_on.extend(s.input_streams.iter().map(|_| want.is_some_and(|w| w.input)));
            out_on.extend(s.output_streams.iter().map(|_| want.is_some_and(|w| w.output)));
        }
        in_on.extend(realized.taps.iter().map(|_| true));
        dev.usage = (in_on, out_on);
        dev.realized = realized;
        Ok(dev)
    }

    fn read_realized(&self, topo: &Topology) -> Realized {
        let agg = self.aggregate;
        let mut subdevices = Vec::new();
        match props::get_ids(agg, AudioObjectPropertyAddress::global(kAudioAggregateDevicePropertyActiveSubDeviceList)) {
            Ok(ids) if !ids.is_empty() => {
                for id in ids {
                    let Some(uid) = device_uid(id) else { continue };
                    subdevices.push(RealizedSub {
                        uid,
                        input_streams: stream_channels(id, kAudioObjectPropertyScopeInput),
                        output_streams: stream_channels(id, kAudioObjectPropertyScopeOutput),
                    });
                }
            }
            _ => {
                for s in &topo.subdevices {
                    let Some(id) = id_for_uid(&s.uid) else { continue };
                    subdevices.push(RealizedSub {
                        uid: s.uid.clone(),
                        input_streams: stream_channels(id, kAudioObjectPropertyScopeInput),
                        output_streams: stream_channels(id, kAudioObjectPropertyScopeOutput),
                    });
                }
            }
        }
        let tap_channels = |id: AudioObjectID| {
            props::get::<AudioStreamBasicDescription>(id, AudioObjectPropertyAddress::global(kAudioTapPropertyFormat))
                .map(|f| f.mChannelsPerFrame)
                .unwrap_or(2)
        };
        let taps = match props::get_ids(agg, AudioObjectPropertyAddress::global(kAudioAggregateDevicePropertySubTapList)) {
            Ok(ids) if !ids.is_empty() => {
                // Sub-tap objects of the aggregate; map them to our taps by
                // uid.
                let ours: Vec<String> = self
                    .taps
                    .iter()
                    .map(|t| props::get_string(*t, AudioObjectPropertyAddress::global(kAudioTapPropertyUID)).unwrap_or_default())
                    .collect();
                ids.iter()
                    .filter_map(|id| {
                        let uid = props::get_string(*id, AudioObjectPropertyAddress::global(kAudioTapPropertyUID)).ok()?;
                        let i = ours.iter().position(|u| *u == uid).or_else(|| self.taps.iter().position(|t| t == id))?;
                        Some((i, tap_channels(*id)))
                    })
                    .collect()
            }
            _ => self.taps.iter().enumerate().map(|(i, t)| (i, tap_channels(*t))).collect(),
        };
        Realized { subdevices, taps, sample_rate: nominal_rate(agg) }
    }

    fn set_stream_usage(&self, scope: u32, on: &[bool]) {
        if on.is_empty() || on.iter().all(|b| *b) {
            return;
        }
        // struct AudioHardwareIOProcStreamUsage { void* mIOProc; UInt32 mNumberStreams; UInt32 mStreamIsOn[]; }
        // (64-bit: pointer at 0, count at 8, flags from 12). 8-byte aligned buffer.
        let n = on.len();
        let size = 12 + 4 * n;
        let mut buf = vec![0u64; size.div_ceil(8)];
        unsafe {
            let base = buf.as_mut_ptr() as *mut u8;
            (base as *mut usize).write(self.proc_id as usize);
            (base.add(8) as *mut u32).write(n as u32);
            for (i, b) in on.iter().enumerate() {
                (base.add(12 + 4 * i) as *mut u32).write_unaligned(*b as u32);
            }
        }
        let size = size as u32;
        let addr = AudioObjectPropertyAddress::new(kAudioDevicePropertyIOProcStreamUsage, scope, kAudioObjectPropertyElementMain);
        let st = unsafe { AudioObjectSetPropertyData(self.aggregate, &addr, 0, std::ptr::null(), size, buf.as_ptr() as *const c_void) };
        if st != 0 {
            log::debug!("audio-control: mixer stream usage: {}", status_str(st));
        }
    }
}

impl MixDevice for MacMixDevice {
    fn realized(&self) -> &Realized {
        &self.realized
    }

    fn start(&mut self, graph: Box<Graph>) -> Result<()> {
        if !self.ctx.is_null() {
            return Err(anyhow!("already started"));
        }
        self.ctx = Box::into_raw(Box::new(IoCtx { graph, poisoned: AtomicBool::new(false) }));
        let mut pid: AudioDeviceIOProcID = std::ptr::null_mut();
        check(unsafe { AudioDeviceCreateIOProcID(self.aggregate, mix_io_proc, self.ctx as *mut c_void, &mut pid) }, "AudioDeviceCreateIOProcID")?;
        self.proc_id = pid;
        let (ins, outs) = self.usage.clone();
        self.set_stream_usage(kAudioObjectPropertyScopeInput, &ins);
        self.set_stream_usage(kAudioObjectPropertyScopeOutput, &outs);
        check(unsafe { AudioDeviceStart(self.aggregate, pid) }, "AudioDeviceStart")
    }

    fn diagnostics(&self) -> Vec<String> {
        let mut v = vec![format!(
            "mixer device {} (id {}): {} sub-device(s), {} tap(s), inputs {:?}, outputs {:?}",
            self.name,
            self.aggregate,
            self.realized.subdevices.len(),
            self.taps.len(),
            self.realized.input_streams(),
            self.realized.output_streams()
        )];
        if !self.ctx.is_null() && unsafe { (*self.ctx).poisoned.load(Ordering::Relaxed) } {
            v.push("mixer IOProc panicked; it outputs silence".into());
        }
        v
    }
}

impl Drop for MacMixDevice {
    fn drop(&mut self) {
        unsafe {
            if !self.proc_id.is_null() {
                AudioDeviceStop(self.aggregate, self.proc_id);
                AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id);
            }
            if self.aggregate != 0 {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            // Only after the IOProc is gone.
            if !self.ctx.is_null() {
                drop(Box::from_raw(self.ctx));
            }
        }
        for t in self.taps.drain(..) {
            destroy_tap_object(t);
        }
    }
}

/// `AudioBufferList`s as [`IoBuffers`].
pub(crate) struct AblIo<'a> {
    pub ins: &'a [AudioBuffer],
    pub outs: &'a mut [AudioBuffer],
}

impl IoBuffers for AblIo<'_> {
    fn input(&self, i: usize) -> Option<(&[f32], usize)> {
        let b = self.ins.get(i)?;
        if b.mData.is_null() || b.mNumberChannels == 0 {
            return None;
        }
        Some((unsafe { std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4) }, b.mNumberChannels as usize))
    }

    fn output_streams(&self) -> usize {
        self.outs.len()
    }

    fn output(&mut self, i: usize) -> Option<(&mut [f32], usize)> {
        let b = self.outs.get_mut(i)?;
        if b.mData.is_null() || b.mNumberChannels == 0 {
            return None;
        }
        Some((unsafe { std::slice::from_raw_parts_mut(b.mData as *mut f32, b.mDataByteSize as usize / 4) }, b.mNumberChannels as usize))
    }
}

/// Frames in this callback: from the first non-empty output, else input.
pub(crate) fn callback_frames(ins: &[AudioBuffer], outs: &[AudioBuffer]) -> usize {
    outs.iter()
        .chain(ins.iter())
        .find(|b| !b.mData.is_null() && b.mNumberChannels > 0 && b.mDataByteSize > 0)
        .map(|b| b.mDataByteSize as usize / 4 / b.mNumberChannels as usize)
        .unwrap_or(0)
}

/// The mixer IOProc. Real-time safe: no allocation, locks or syscalls; a
/// panic (which would be a bug) silences the mixer instead of aborting
/// coreaudiod's client.
pub(crate) unsafe extern "C" fn mix_io_proc(
    _device: AudioObjectID,
    _now: *const AudioTimeStamp,
    input: *const AudioBufferList,
    _input_time: *const AudioTimeStamp,
    output: *mut AudioBufferList,
    _output_time: *const AudioTimeStamp,
    data: *mut c_void,
) -> OSStatus {
    if data.is_null() {
        return 0;
    }
    let ctx = &mut *(data as *mut IoCtx);
    let ins = AudioBufferList::buffers(input);
    let outs = AudioBufferList::buffers_mut(output);
    if ctx.poisoned.load(Ordering::Relaxed) {
        for b in outs.iter_mut() {
            if !b.mData.is_null() {
                std::ptr::write_bytes(b.mData as *mut u8, 0, b.mDataByteSize as usize);
            }
        }
        return 0;
    }
    let frames = callback_frames(ins, outs);
    let graph = &mut ctx.graph;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut io = AblIo { ins, outs };
        graph.process(&mut io, frames);
    }));
    if r.is_err() {
        ctx.poisoned.store(true, Ordering::Relaxed);
    }
    0
}

/// Test hooks: an IOProc client-data pointer for a graph, and freeing it.
#[cfg(test)]
pub(crate) fn test_ctx(graph: Box<Graph>) -> *mut c_void {
    Box::into_raw(Box::new(IoCtx { graph, poisoned: AtomicBool::new(false) })) as *mut c_void
}

#[cfg(test)]
pub(crate) fn free_test_ctx(ctx: *mut c_void) -> Box<Graph> {
    let ctx = unsafe { Box::from_raw(ctx as *mut IoCtx) };
    assert!(!ctx.poisoned.load(Ordering::Relaxed), "the IOProc panicked");
    ctx.graph
}

#[cfg(test)]
pub(crate) fn run_io_proc_for_test(graph: Box<Graph>, input: *const AudioBufferList, output: *mut AudioBufferList) -> Box<Graph> {
    let ctx = test_ctx(graph);
    unsafe { mix_io_proc(0, std::ptr::null(), input, std::ptr::null(), output, std::ptr::null(), ctx) };
    free_test_ctx(ctx)
}
