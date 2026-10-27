//! Windows backend:
//! * DDC/CI through Dxva2 (`GetPhysicalMonitorsFromHMONITOR`,
//!   `GetVCPFeatureAndVCPFeatureReply` / `SetVCPFeature`);
//! * laptop panels through the `\\.\LCD` brightness IOCTLs (the same
//!   driver interface WMI's `WmiMonitorBrightnessMethods` uses);
//! * software dimming with `SetDeviceGammaRamp` (Windows rejects ramps far
//!   from identity unless `GdiIcmGammaRange` is raised, so very low levels
//!   may fail);
//! * the shared `WH_KEYBOARD_LL` hook (`pm-hook`) for the volume keys and hotkeys (brightness
//!   keys are handled by firmware / ACPI on Windows and never reach it).
//!
//! No OSD on Windows yet.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, bail, Result};
use windows_sys::Win32::Devices::Display::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::UI::ColorSystem::{GetDeviceGammaRamp, SetDeviceGammaRamp};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::Mods;
use crate::engine::{Backend, Inner, RawDisplay, ReadError};
use crate::identity::{self, IdentityFacts};
use crate::keys::{hotkey_action, windows_vk_media, Action, KeyRouter, KeySource, MediaKeyEvent, Route};
use crate::Rect;

const ERROR_GRAPHICS_DDCCI_VCP_NOT_SUPPORTED: u32 = 0xC026_2584;

// winioctl.h / ntddvdeo.h
const IOCTL_VIDEO_QUERY_SUPPORTED_BRIGHTNESS: u32 = 0x0023_0494;
const IOCTL_VIDEO_QUERY_DISPLAY_BRIGHTNESS: u32 = 0x0023_0498;
const IOCTL_VIDEO_SET_DISPLAY_BRIGHTNESS: u32 = 0x0023_049C;
const DISPLAYPOLICY_AC: u8 = 1;
const DISPLAYPOLICY_DC: u8 = 2;
const DISPLAYPOLICY_BOTH: u8 = 3;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct DisplayBrightness {
    policy: u8,
    ac: u8,
    dc: u8,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

struct Monitor {
    gdi_name: String,
    physical: Option<HANDLE>,
    builtin: bool,
}

#[derive(Default)]
struct WinBackend {
    monitors: HashMap<String, Monitor>,
    /// All physical monitor handles (destroyed on re-enumeration / drop).
    physical: Vec<PHYSICAL_MONITOR>,
    gamma: HashMap<String, Box<[u16; 768]>>,
}

// Monitor handles are only used on the worker thread.
unsafe impl Send for WinBackend {}

pub(crate) fn new_backend() -> Box<dyn Backend> {
    Box::new(WinBackend::default())
}

struct Target {
    friendly: String,
    device_path: String,
    builtin: bool,
    vendor: u16,
    product: u16,
}

/// GDI source name (`\\.\DISPLAY1`) → its targets (several when cloned).
fn display_config_targets() -> HashMap<String, Vec<Target>> {
    let mut out: HashMap<String, Vec<Target>> = HashMap::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != 0 {
            return out;
        }
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![std::mem::zeroed(); np as usize];
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![std::mem::zeroed(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), std::ptr::null_mut()) != 0 {
            return out;
        }
        for p in &paths[..np as usize] {
            let mut src: DISPLAYCONFIG_SOURCE_DEVICE_NAME = std::mem::zeroed();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let mut tgt: DISPLAYCONFIG_TARGET_DEVICE_NAME = std::mem::zeroed();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut tgt.header) != 0 {
                continue;
            }
            let tech = tgt.outputTechnology;
            let builtin = tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
                || tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
                || tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED
                || tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS;
            out.entry(from_wide(&src.viewGdiDeviceName)).or_default().push(Target {
                friendly: from_wide(&tgt.monitorFriendlyDeviceName),
                device_path: from_wide(&tgt.monitorDevicePath),
                builtin,
                vendor: tgt.edidManufactureId,
                product: tgt.edidProductCodeId,
            });
        }
    }
    out
}

fn read_edid(device_path: &str) -> Option<identity::Edid> {
    let key = wide(&identity::edid_registry_key(device_path)?);
    let value = wide("EDID");
    let mut buf = vec![0u8; 512];
    let mut len = buf.len() as u32;
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_BINARY, std::ptr::null_mut(), buf.as_mut_ptr() as *mut c_void, &mut len) };
    if r != 0 {
        return None;
    }
    identity::parse_edid(&buf[..len as usize])
}

fn monitors() -> Vec<(HMONITOR, String, Rect)> {
    unsafe extern "system" fn cb(hm: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let v = &mut *(data as *mut Vec<HMONITOR>);
        v.push(hm);
        TRUE
    }
    let mut hms: Vec<HMONITOR> = Vec::new();
    unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(cb), &mut hms as *mut _ as LPARAM) };
    hms.into_iter()
        .filter_map(|hm| unsafe {
            let mut mi: MONITORINFOEXW = std::mem::zeroed();
            mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(hm, &mut mi.monitorInfo) == 0 {
                return None;
            }
            let r = mi.monitorInfo.rcMonitor;
            Some((hm, from_wide(&mi.szDevice), Rect { x: r.left, y: r.top, width: r.right - r.left, height: r.bottom - r.top }))
        })
        .collect()
}

/// The `\\.\LCD` device (laptop panel brightness), if present.
struct Lcd(HANDLE);

impl Lcd {
    fn open() -> Option<Self> {
        let name = wide(r"\\.\LCD");
        let h = unsafe { CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
        (h != INVALID_HANDLE_VALUE).then_some(Self(h))
    }

    fn supported_levels(&self) -> Vec<u8> {
        let mut levels = [0u8; 256];
        let mut got = 0u32;
        let ok = unsafe { DeviceIoControl(self.0, IOCTL_VIDEO_QUERY_SUPPORTED_BRIGHTNESS, std::ptr::null(), 0, levels.as_mut_ptr() as *mut c_void, levels.len() as u32, &mut got, std::ptr::null_mut()) };
        if ok == 0 {
            return Vec::new();
        }
        levels[..got as usize].to_vec()
    }

    fn get(&self) -> Option<u8> {
        let mut b = DisplayBrightness::default();
        let mut got = 0u32;
        let ok = unsafe {
            DeviceIoControl(self.0, IOCTL_VIDEO_QUERY_DISPLAY_BRIGHTNESS, std::ptr::null(), 0, &mut b as *mut _ as *mut c_void, std::mem::size_of::<DisplayBrightness>() as u32, &mut got, std::ptr::null_mut())
        };
        if ok == 0 {
            return None;
        }
        let mut ps: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        let on_ac = unsafe { GetSystemPowerStatus(&mut ps) } == 0 || ps.ACLineStatus != 0;
        Some(match b.policy {
            DISPLAYPOLICY_DC => b.dc,
            DISPLAYPOLICY_AC => b.ac,
            _ if on_ac => b.ac,
            _ => b.dc,
        })
    }

    fn set(&self, pct: u8) -> Result<()> {
        let levels = self.supported_levels();
        let level = levels.iter().copied().min_by_key(|&l| (l as i32 - pct as i32).abs()).unwrap_or(pct);
        let b = DisplayBrightness { policy: DISPLAYPOLICY_BOTH, ac: level, dc: level };
        let mut got = 0u32;
        let ok = unsafe {
            DeviceIoControl(self.0, IOCTL_VIDEO_SET_DISPLAY_BRIGHTNESS, &b as *const _ as *const c_void, std::mem::size_of::<DisplayBrightness>() as u32, std::ptr::null_mut(), 0, &mut got, std::ptr::null_mut())
        };
        if ok == 0 {
            bail!("IOCTL_VIDEO_SET_DISPLAY_BRIGHTNESS failed: {}", unsafe { GetLastError() });
        }
        Ok(())
    }
}

impl Drop for Lcd {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl WinBackend {
    fn release_physical(&mut self) {
        if !self.physical.is_empty() {
            unsafe { DestroyPhysicalMonitors(self.physical.len() as u32, self.physical.as_ptr()) };
            self.physical.clear();
        }
    }

    fn handle(&self, id: &str) -> Result<HANDLE> {
        self.monitors.get(id).and_then(|m| m.physical).ok_or_else(|| anyhow!("{id:?} has no DDC handle"))
    }

    fn with_dc<T>(&self, id: &str, f: impl FnOnce(HDC) -> Result<T>) -> Result<T> {
        let m = self.monitors.get(id).ok_or_else(|| anyhow!("unknown display {id:?}"))?;
        let dev = wide(&m.gdi_name);
        let hdc = unsafe { CreateDCW(std::ptr::null(), dev.as_ptr(), std::ptr::null(), std::ptr::null()) };
        if hdc.is_null() {
            bail!("CreateDCW({}) failed", m.gdi_name);
        }
        let r = f(hdc);
        unsafe { DeleteDC(hdc) };
        r
    }
}

impl Backend for WinBackend {
    fn enumerate(&mut self) -> Vec<RawDisplay> {
        self.release_physical();
        self.monitors.clear();
        let targets = display_config_targets();
        let has_lcd = Lcd::open().map(|l| l.get().is_some()).unwrap_or(false);
        struct Found {
            facts: IdentityFacts,
            gdi: String,
            bounds: Rect,
            physical: Option<HANDLE>,
        }
        let mut found: Vec<Found> = Vec::new();
        for (hm, gdi, bounds) in monitors() {
            let mut n = 0u32;
            let mut phys: Vec<PHYSICAL_MONITOR> = Vec::new();
            if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hm, &mut n) } != 0 && n > 0 {
                phys = vec![unsafe { std::mem::zeroed() }; n as usize];
                if unsafe { GetPhysicalMonitorsFromHMONITOR(hm, n, phys.as_mut_ptr()) } == 0 {
                    phys.clear();
                }
            }
            let tg = targets.get(&gdi).map(|v| v.as_slice()).unwrap_or(&[]);
            let count = phys.len().max(tg.len()).max(1);
            for i in 0..count {
                let t = tg.get(i);
                let p = phys.get(i);
                let edid = t.and_then(|t| read_edid(&t.device_path));
                let name = t
                    .map(|t| t.friendly.clone())
                    .filter(|s| !s.is_empty())
                    .or_else(|| edid.as_ref().and_then(|e| e.name.clone()))
                    .or_else(|| {
                        p.map(|p| {
                            let desc = p.szPhysicalMonitorDescription;
                            from_wide(&desc)
                        })
                    })
                    .unwrap_or_else(|| gdi.clone());
                let serial = edid.as_ref().and_then(|e| e.serial_string.clone().or_else(|| (e.serial_number != 0).then(|| e.serial_number.to_string())));
                found.push(Found {
                    facts: IdentityFacts {
                        name,
                        serial,
                        vendor: t.map(|t| t.vendor as u32).unwrap_or(0),
                        model: t.map(|t| t.product as u32).unwrap_or(0),
                        is_builtin: t.map(|t| t.builtin).unwrap_or(false),
                    },
                    gdi: gdi.clone(),
                    bounds,
                    physical: p.map(|p| p.hPhysicalMonitor),
                });
            }
            self.physical.extend(phys);
        }
        let facts: Vec<IdentityFacts> = found.iter().map(|f| f.facts.clone()).collect();
        let ids = identity::stable_ids(&facts);
        let mut out = Vec::new();
        for (f, id) in found.into_iter().zip(ids) {
            let builtin = f.facts.is_builtin;
            // Laptop panels rarely speak DDC; use the IOCTL for them.
            let ddc = f.physical.is_some() && !builtin;
            out.push(RawDisplay { id: id.clone(), name: f.facts.name.clone(), is_builtin: builtin, bounds: f.bounds, ddc, native_brightness: builtin && has_lcd, gamma: true });
            self.monitors.insert(id, Monitor { gdi_name: f.gdi, physical: f.physical, builtin });
        }
        out
    }

    fn signature(&mut self) -> u64 {
        let mut h = DefaultHasher::new();
        for (_, gdi, r) in monitors() {
            gdi.hash(&mut h);
            (r.x, r.y, r.width, r.height).hash(&mut h);
        }
        h.finish()
    }

    fn ddc_read(&mut self, id: &str, vcp: u8) -> Result<(u16, u16), ReadError> {
        let h = self.handle(id).map_err(|_| ReadError::Unsupported)?;
        let (mut kind, mut cur, mut max) = (0, 0u32, 0u32);
        if unsafe { GetVCPFeatureAndVCPFeatureReply(h, vcp, &mut kind, &mut cur, &mut max) } == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_GRAPHICS_DDCCI_VCP_NOT_SUPPORTED {
                return Err(ReadError::Unsupported);
            }
            return Err(ReadError::Transient(format!("GetVCPFeatureAndVCPFeatureReply(0x{vcp:02X}) failed: 0x{err:08X}")));
        }
        Ok((cur.min(u16::MAX as u32) as u16, max.min(u16::MAX as u32) as u16))
    }

    fn ddc_write(&mut self, id: &str, vcp: u8, value: u16) -> Result<()> {
        let h = self.handle(id)?;
        if unsafe { SetVCPFeature(h, vcp, value as u32) } == 0 {
            bail!("SetVCPFeature(0x{vcp:02X}) failed: 0x{:08X}", unsafe { GetLastError() });
        }
        Ok(())
    }

    fn native_get(&mut self, id: &str) -> Option<f32> {
        if !self.monitors.get(id)?.builtin {
            return None;
        }
        Lcd::open()?.get().map(|v| v.min(100) as f32 / 100.0)
    }

    fn native_set(&mut self, id: &str, level: f32) -> Result<()> {
        if !self.monitors.get(id).is_some_and(|m| m.builtin) {
            bail!("{id:?} is not the built-in panel");
        }
        let lcd = Lcd::open().ok_or_else(|| anyhow!(r"\\.\LCD not available"))?;
        lcd.set((level.clamp(0.0, 1.0) * 100.0).round() as u8)
    }

    fn gamma_set(&mut self, id: &str, factor: f32) -> Result<()> {
        if !self.gamma.contains_key(id) {
            let mut ramp = Box::new([0u16; 768]);
            self.with_dc(id, |hdc| {
                if unsafe { GetDeviceGammaRamp(hdc, ramp.as_mut_ptr() as *mut c_void) } == 0 {
                    bail!("GetDeviceGammaRamp failed");
                }
                Ok(())
            })?;
            self.gamma.insert(id.to_string(), ramp);
        }
        let orig = self.gamma[id].clone();
        let f = factor.clamp(0.0, 1.0);
        let mut ramp = orig.clone();
        for v in ramp.iter_mut() {
            *v = (*v as f32 * f).round() as u16;
        }
        self.with_dc(id, |hdc| {
            if unsafe { SetDeviceGammaRamp(hdc, ramp.as_ptr() as *const c_void) } == 0 {
                bail!("SetDeviceGammaRamp refused the ramp (Windows limits how far gamma may deviate)");
            }
            Ok(())
        })?;
        if f >= 1.0 {
            self.gamma.remove(id);
        }
        Ok(())
    }

    fn gamma_restore(&mut self) {
        let saved: Vec<(String, Box<[u16; 768]>)> = self.gamma.drain().collect();
        for (id, ramp) in saved {
            let _ = self.with_dc(&id, |hdc| {
                unsafe { SetDeviceGammaRamp(hdc, ramp.as_ptr() as *const c_void) };
                Ok(())
            });
        }
    }
}

impl Drop for WinBackend {
    fn drop(&mut self) {
        self.gamma_restore();
        self.release_physical();
    }
}

// ---- keyboard hook -------------------------------------------------------

struct HookCtx {
    inner: Arc<Inner>,
    router: KeyRouter,
    held: Vec<u32>,
    hotkey_downs: Vec<u32>,
}

static HOOK_CTX: Mutex<Option<HookCtx>> = Mutex::new(None);

const VK_SHIFT: i32 = 0x10;
const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;
const VK_LWIN: i32 = 0x5B;
const VK_RWIN: i32 = 0x5C;
const LLKHF_INJECTED: u32 = 0x10;

fn key_down(vk: i32) -> bool {
    (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
}

fn current_mods() -> Mods {
    Mods { win: key_down(VK_LWIN) || key_down(VK_RWIN), ctrl: key_down(VK_CONTROL), alt: key_down(VK_MENU), shift: key_down(VK_SHIFT) }
}

fn cursor_display(inner: &Inner) -> Option<String> {
    let mut p = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut p) } == 0 {
        return None;
    }
    inner.display_at(p.x as f64, p.y as f64)
}

/// Returns true to swallow.
fn on_key(ctx: &mut HookCtx, vk: u32, down: bool) -> bool {
    if !ctx.inner.lock().config.enabled {
        return false;
    }
    let mods = current_mods();
    if let Some(key) = windows_vk_media(vk) {
        let repeat = down && ctx.held.contains(&vk);
        if down && !repeat {
            ctx.held.push(vk);
        } else if !down {
            ctx.held.retain(|&k| k != vk);
        }
        let target = if down { cursor_display(&ctx.inner) } else { None };
        let inner = ctx.inner.clone();
        let route = ctx.router.route(MediaKeyEvent { key, down, repeat }, KeySource::KeyCode, || target.as_deref().is_some_and(|t| inner.takes_over(t, key)), Instant::now());
        return match route {
            Route::Pass => false,
            Route::Swallow => true,
            Route::Act => {
                if let Some(t) = target {
                    let _ = ctx.inner.perform(&t, Action::for_media_key(key, mods));
                }
                true
            }
        };
    }
    if !down {
        if let Some(i) = ctx.hotkey_downs.iter().position(|&k| k == vk) {
            ctx.hotkey_downs.remove(i);
            return true;
        }
        return false;
    }
    let action = hotkey_action(&ctx.inner.lock().config, vk, mods);
    let Some(action) = action else { return false };
    if !ctx.hotkey_downs.contains(&vk) {
        ctx.hotkey_downs.push(vk);
    }
    if let Some(t) = cursor_display(&ctx.inner) {
        let _ = ctx.inner.perform(&t, action);
    }
    true
}

/// The old `hook_proc` body: true = swallow.
fn key_event(msg: u32, k: &KBDLLHOOKSTRUCT) -> bool {
    if k.flags & LLKHF_INJECTED != 0 {
        return false;
    }
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut g = HOOK_CTX.lock().unwrap_or_else(|e| e.into_inner());
        g.as_mut().map(|ctx| on_key(ctx, k.vkCode, down)).unwrap_or(false)
    }))
    .unwrap_or(false)
}

/// The subscription to the shared keyboard hook (`pm-hook`,
/// [`pm_hook::priority::DISPLAY_CONTROL`]).
pub(crate) struct KeyHook {
    guard: Option<pm_hook::Guard>,
}

impl KeyHook {
    pub(crate) fn start(inner: Arc<Inner>) -> Result<Self> {
        use pm_hook::{EventMask, Subscription, Verdict};
        {
            let mut g = HOOK_CTX.lock().unwrap_or_else(|e| e.into_inner());
            if g.is_some() {
                bail!("a display-control key hook is already running");
            }
            *g = Some(HookCtx { inner, router: KeyRouter::default(), held: Vec::new(), hotkey_downs: Vec::new() });
        }
        let guard = pm_hook::subscribe(Subscription::new("display-control", pm_hook::priority::DISPLAY_CONTROL, EventMask::KEYS, |ev| {
            match ev.keyboard() {
                Some((msg, k)) if key_event(msg, k) => Verdict::Swallow,
                _ => Verdict::Pass,
            }
        }));
        if !pm_hook::status().is_active() {
            drop(guard);
            *HOOK_CTX.lock().unwrap_or_else(|e| e.into_inner()) = None;
            bail!("could not install the keyboard hook");
        }
        Ok(Self { guard: Some(guard) })
    }

    pub(crate) fn stop(&mut self) {
        if self.guard.take().is_some() {
            *HOOK_CTX.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

impl Drop for KeyHook {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn close_osd() {}
