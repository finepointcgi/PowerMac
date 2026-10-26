//! Windows backend: MMDevice API for devices and notifications, the
//! undocumented-but-stable `IPolicyConfig` for setting default endpoints
//! (what every "audio switcher" uses), `IAudioEndpointVolume` for device
//! volume/mute and audio sessions (`ISimpleAudioVolume`) for per-app volume.
//!
//! Compile-checked only; not yet run on Windows.
#![allow(non_snake_case)]

use crate::rules::Snapshot;
use crate::{AppLevel, AudioApp, AudioDevice, Backend, CapturePermission, DeviceKind, Transport};
use anyhow::{anyhow, Result};
use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use windows::Win32::Devices::FunctionDiscovery::{PKEY_Device_EnumeratorName, PKEY_Device_FriendlyName};
use windows::Win32::Foundation::{CloseHandle, PROPERTYKEY};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, eMultimedia, eRender, AudioSessionStateActive, EDataFlow, ERole,
    IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE, DEVICE_STATE_ACTIVE,
    PKEY_AudioEndpoint_FormFactor,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows_core::{implement, interface, IUnknown, IUnknown_Vtbl, Interface, GUID, HRESULT, PCWSTR, PWSTR};

#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn GetMixFormat(&self, id: PCWSTR, format: *mut *mut c_void) -> HRESULT;
    fn GetDeviceFormat(&self, id: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
    fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(&self, id: PCWSTR, endpoint: *mut c_void, mix: *mut c_void) -> HRESULT;
    fn GetProcessingPeriod(&self, id: PCWSTR, default: i32, def: *mut i64, min: *mut i64) -> HRESULT;
    fn SetProcessingPeriod(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
    fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn GetPropertyValue(&self, id: PCWSTR, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
    fn SetPropertyValue(&self, id: PCWSTR, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
    fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
}

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

thread_local! {
    static COM: () = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    };
}

fn com() {
    COM.with(|_| ());
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    com();
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}

fn pwstr_string(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn device_id(d: &IMMDevice) -> String {
    unsafe { d.GetId().map(pwstr_string).unwrap_or_default() }
}

fn prop_string(d: &IMMDevice, key: &PROPERTYKEY) -> String {
    unsafe {
        d.OpenPropertyStore(STGM_READ)
            .and_then(|s| s.GetValue(key))
            .map(|v| v.to_string())
            .unwrap_or_default()
    }
}

fn transport_of(d: &IMMDevice) -> Transport {
    let en = prop_string(d, &PKEY_Device_EnumeratorName).to_ascii_uppercase();
    let form: u32 = prop_string(d, &PKEY_AudioEndpoint_FormFactor).trim().parse().unwrap_or(10);
    if en.contains("BTH") {
        Transport::Bluetooth
    } else if en.contains("USB") {
        Transport::Usb
    } else if form == 9 {
        Transport::Hdmi
    } else if en.contains("HDAUDIO") || en.contains("INTELAUDIO") || en.contains("ACPI") {
        if form == 0 {
            Transport::Unknown
        } else {
            Transport::BuiltIn
        }
    } else if en.starts_with("SWD") || en.starts_with("ROOT") {
        Transport::Virtual
    } else {
        Transport::Unknown
    }
}

fn endpoint_volume(d: &IMMDevice) -> Option<IAudioEndpointVolume> {
    unsafe { d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok() }
}

fn list(e: &IMMDeviceEnumerator, flow: EDataFlow, default_id: &Option<String>, sys_id: &Option<String>, levels: bool) -> Vec<AudioDevice> {
    let mut out = Vec::new();
    let Ok(coll) = (unsafe { e.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }) else { return out };
    let n = unsafe { coll.GetCount() }.unwrap_or(0);
    for i in 0..n {
        let Ok(d) = (unsafe { coll.Item(i) }) else { continue };
        let uid = device_id(&d);
        let vol = if levels { endpoint_volume(&d) } else { None };
        out.push(AudioDevice {
            is_default: default_id.as_deref() == Some(uid.as_str()),
            is_default_system: flow == eRender && sys_id.as_deref() == Some(uid.as_str()),
            name: prop_string(&d, &PKEY_Device_FriendlyName),
            transport: transport_of(&d),
            channels: vol.as_ref().and_then(|v| unsafe { v.GetChannelCount() }.ok()).unwrap_or(0),
            volume: vol.as_ref().and_then(|v| unsafe { v.GetMasterVolumeLevelScalar() }.ok()),
            muted: vol.as_ref().and_then(|v| unsafe { v.GetMute() }.ok()).map(|b| b.as_bool()),
            uid,
        });
    }
    out
}

fn default_id(e: &IMMDeviceEnumerator, flow: EDataFlow, role: ERole) -> Option<String> {
    unsafe { e.GetDefaultAudioEndpoint(flow, role) }.ok().map(|d| device_id(&d))
}

fn exe_of(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().map(|s| s.to_string())
    }
}

struct Session {
    instance: String,
    pid: u32,
    exe: String,
    playing: bool,
    volume: ISimpleAudioVolume,
}

fn sessions() -> Result<Vec<Session>> {
    let e = enumerator()?;
    let mut out = Vec::new();
    let coll = unsafe { e.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
    for i in 0..unsafe { coll.GetCount()? } {
        let Ok(dev) = (unsafe { coll.Item(i) }) else { continue };
        let Ok(mgr) = (unsafe { dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }) else { continue };
        let Ok(en) = (unsafe { mgr.GetSessionEnumerator() }) else { continue };
        for j in 0..unsafe { en.GetCount() }.unwrap_or(0) {
            let Ok(ctl) = (unsafe { en.GetSession(j) }) else { continue };
            let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else { continue };
            if unsafe { ctl2.IsSystemSoundsSession() } == windows_core::HRESULT(0) {
                continue;
            }
            let pid = unsafe { ctl2.GetProcessId() }.unwrap_or(0);
            if pid == 0 {
                continue;
            }
            let Some(exe) = exe_of(pid) else { continue };
            let Ok(volume) = ctl.cast::<ISimpleAudioVolume>() else { continue };
            let instance = unsafe { ctl2.GetSessionInstanceIdentifier() }.map(pwstr_string).unwrap_or_default();
            let playing = unsafe { ctl.GetState() }.map(|s| s == AudioSessionStateActive).unwrap_or(false);
            out.push(Session { instance, pid, exe, playing, volume });
        }
    }
    Ok(out)
}

#[implement(IMMNotificationClient)]
struct Notifier {
    f: Arc<dyn Fn() + Send + Sync>,
}

impl IMMNotificationClient_Impl for Notifier_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> windows_core::Result<()> {
        (self.f)();
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows_core::Result<()> {
        (self.f)();
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows_core::Result<()> {
        (self.f)();
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, _flow: EDataFlow, _role: ERole, _id: &PCWSTR) -> windows_core::Result<()> {
        (self.f)();
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> windows_core::Result<()> {
        Ok(())
    }
}

struct WatchGuard {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

// Both objects live in the multithreaded apartment.
unsafe impl Send for WatchGuard {}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        com();
        unsafe {
            let _ = self.enumerator.UnregisterEndpointNotificationCallback(&self.client);
        }
    }
}

pub(crate) struct WinBackend {
    /// What we last set per session instance, so the user's own mixer
    /// changes are not fought.
    applied: Mutex<HashMap<String, AppLevel>>,
}

impl WinBackend {
    pub(crate) fn new() -> Result<Self> {
        enumerator()?;
        Ok(WinBackend { applied: Mutex::new(HashMap::new()) })
    }

    fn device(&self, uid: &str) -> Result<IMMDevice> {
        let e = enumerator()?;
        let w = wide(uid);
        Ok(unsafe { e.GetDevice(PCWSTR(w.as_ptr()))? })
    }
}

impl Backend for WinBackend {
    fn snapshot(&self, levels: bool) -> Result<Snapshot> {
        let e = enumerator()?;
        let out_id = default_id(&e, eRender, eConsole);
        let in_id = default_id(&e, eCapture, eConsole);
        let sys_id = default_id(&e, eRender, eCommunications);
        Ok(Snapshot {
            outputs: list(&e, eRender, &out_id, &sys_id, levels),
            inputs: list(&e, eCapture, &in_id, &None, levels),
            default_output: out_id,
            default_input: in_id,
            system_output: sys_id,
        })
    }

    fn set_default(&self, kind: DeviceKind, uid: &str) -> Result<()> {
        com();
        let policy: IPolicyConfig = unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)? };
        let roles: &[ERole] = match kind {
            DeviceKind::Output => &[eConsole, eMultimedia],
            DeviceKind::Input => &[eConsole, eMultimedia, eCommunications],
            DeviceKind::SystemOutput => &[eCommunications],
        };
        let w = wide(uid);
        for role in roles {
            unsafe { policy.SetDefaultEndpoint(PCWSTR(w.as_ptr()), *role).ok()? };
        }
        Ok(())
    }

    fn set_volume(&self, uid: &str, v: f32) -> Result<()> {
        let d = self.device(uid)?;
        let ev = endpoint_volume(&d).ok_or_else(|| anyhow!("no volume control"))?;
        unsafe { ev.SetMasterVolumeLevelScalar(v, std::ptr::null())? };
        Ok(())
    }

    fn set_mute(&self, uid: &str, m: bool) -> Result<()> {
        let d = self.device(uid)?;
        let ev = endpoint_volume(&d).ok_or_else(|| anyhow!("no volume control"))?;
        unsafe { ev.SetMute(m, std::ptr::null())? };
        Ok(())
    }

    fn watch(&self, notify: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn Send>> {
        let enumerator = enumerator()?;
        let client: IMMNotificationClient = Notifier { f: notify }.into();
        unsafe { enumerator.RegisterEndpointNotificationCallback(&client)? };
        Ok(Box::new(WatchGuard { enumerator, client }))
    }

    fn per_app_supported(&self) -> bool {
        true
    }

    fn max_app_volume(&self) -> f32 {
        1.0
    }

    fn apps(&self, levels: &BTreeMap<String, AppLevel>) -> Result<Vec<AudioApp>> {
        let mut apps: BTreeMap<String, AudioApp> = BTreeMap::new();
        for s in sessions()? {
            let id = s.exe.to_lowercase();
            let vol = unsafe { s.volume.GetMasterVolume() }.unwrap_or(1.0);
            let muted = unsafe { s.volume.GetMute() }.map(|b| b.as_bool()).unwrap_or(false);
            let a = apps.entry(id.clone()).or_insert_with(|| AudioApp {
                pid: s.pid as i32,
                pids: Vec::new(),
                bundle_id: id.clone(),
                name: s.exe.trim_end_matches(".exe").trim_end_matches(".EXE").to_string(),
                volume: levels.get(&id).map(|l| l.volume).unwrap_or(vol),
                muted: levels.get(&id).map(|l| l.muted).unwrap_or(muted),
                playing: false,
            });
            if !a.pids.contains(&(s.pid as i32)) {
                a.pids.push(s.pid as i32);
            }
            a.playing |= s.playing;
        }
        Ok(apps.into_values().collect())
    }

    fn apply_app_levels(&self, levels: &BTreeMap<String, AppLevel>, enabled: bool) -> Result<()> {
        if !enabled {
            return Ok(());
        }
        let mut applied = self.applied.lock().unwrap();
        let sessions = sessions()?;
        let mut live = std::collections::HashSet::new();
        for s in sessions {
            live.insert(s.instance.clone());
            let id = s.exe.to_lowercase();
            let want = levels.get(&id).copied();
            let last = applied.get(&s.instance).copied();
            let target = match (want, last) {
                (Some(w), l) if l != Some(w) => w,
                // Level removed: put the session back to 100 % once.
                (None, Some(_)) => AppLevel::default(),
                _ => continue,
            };
            unsafe {
                s.volume.SetMasterVolume(target.volume.clamp(0.0, 1.0), std::ptr::null())?;
                s.volume.SetMute(target.muted, std::ptr::null())?;
            }
            if want.is_some() {
                applied.insert(s.instance, target);
            } else {
                applied.remove(&s.instance);
            }
        }
        applied.retain(|k, _| live.contains(k));
        Ok(())
    }

    fn capture_permission(&self) -> CapturePermission {
        CapturePermission::NotApplicable
    }

    fn request_capture_permission(&self) -> Result<bool> {
        Ok(true)
    }

    fn app_diagnostics(&self) -> Vec<String> {
        self.applied.lock().unwrap().iter().map(|(k, v)| format!("{k}: {:.0}% muted={}", v.volume * 100.0, v.muted)).collect()
    }
}
