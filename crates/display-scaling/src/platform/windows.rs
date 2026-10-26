//! Windows backend: modes via `EnumDisplaySettingsExW`, switching via
//! `ChangeDisplaySettingsExW` (validated with `CDS_TEST` first; dynamic for
//! the revert window, `CDS_UPDATEREGISTRY` when made permanent), and the
//! per-monitor effective DPI (read-only: Windows scales natively, there is
//! no HiDPI-by-supersampling to add).

use std::collections::HashMap;
use std::ffi::c_void;

use anyhow::{anyhow, bail, Result};
use display_control::identity::{self, IdentityFacts};
use windows_sys::Win32::Devices::Display::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use crate::engine::{Backend, RawDisplay};
use crate::logic::{self, Target, VirtualPlan};
use crate::Mode;

pub(crate) fn hidpi_supported() -> bool {
    false
}

pub(crate) fn new_backend() -> Box<dyn Backend> {
    Box::new(WinBackend::default())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

struct TargetInfo {
    friendly: String,
    device_path: String,
    builtin: bool,
    vendor: u16,
    product: u16,
}

/// GDI source name (`\\.\DISPLAY1`) → its first target (monitor).
fn display_config_targets() -> HashMap<String, TargetInfo> {
    let mut out = HashMap::new();
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
            out.entry(from_wide(&src.viewGdiDeviceName)).or_insert(TargetInfo {
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

/// Monitors: (HMONITOR, GDI device name).
fn monitors() -> Vec<(HMONITOR, String)> {
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
            Some((hm, from_wide(&mi.szDevice)))
        })
        .collect()
}

fn devmode() -> DEVMODEW {
    let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    dm
}

fn mode_of(dm: &DEVMODEW) -> Mode {
    Mode { width: dm.dmPelsWidth, height: dm.dmPelsHeight, pixel_width: dm.dmPelsWidth, pixel_height: dm.dmPelsHeight, refresh: dm.dmDisplayFrequency as f64, hidpi: false }
}

/// All 32-bit modes of a GDI device (`\\.\DISPLAY1`), raw.
fn device_modes(gdi: &str) -> Vec<DEVMODEW> {
    let name = wide(gdi);
    let mut out = Vec::new();
    let mut i = 0u32;
    loop {
        let mut dm = devmode();
        if unsafe { EnumDisplaySettingsExW(name.as_ptr(), i, &mut dm, 0) } == 0 {
            break;
        }
        if dm.dmBitsPerPel >= 24 || dm.dmBitsPerPel == 0 {
            out.push(dm);
        }
        i += 1;
        if i > 4096 {
            break;
        }
    }
    out
}

fn current_devmode(gdi: &str) -> Option<DEVMODEW> {
    let name = wide(gdi);
    let mut dm = devmode();
    (unsafe { EnumDisplaySettingsExW(name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm, 0) } != 0).then_some(dm)
}

fn disp_change_name(r: i32) -> &'static str {
    match r {
        DISP_CHANGE_SUCCESSFUL => "successful",
        DISP_CHANGE_RESTART => "restart required",
        DISP_CHANGE_BADMODE => "mode not supported",
        DISP_CHANGE_BADFLAGS => "bad flags",
        DISP_CHANGE_BADPARAM => "bad parameter",
        DISP_CHANGE_FAILED => "driver failed",
        DISP_CHANGE_NOTUPDATED => "registry not updated",
        DISP_CHANGE_BADDUALVIEW => "bad dual view",
        _ => "unknown error",
    }
}

#[derive(Default)]
struct WinBackend {
    /// Stable id → GDI device name.
    gdi: HashMap<String, String>,
}

impl WinBackend {
    fn found(&mut self) -> Vec<(String, IdentityFacts, HMONITOR, String)> {
        let targets = display_config_targets();
        let mons = monitors();
        let facts: Vec<IdentityFacts> = mons
            .iter()
            .map(|(_, gdi)| {
                let t = targets.get(gdi);
                let edid = t.and_then(|t| read_edid(&t.device_path));
                let name = t.map(|t| t.friendly.clone()).filter(|s| !s.is_empty()).or_else(|| edid.as_ref().and_then(|e| e.name.clone())).unwrap_or_else(|| gdi.clone());
                let serial = edid.as_ref().and_then(|e| e.serial_string.clone().or_else(|| (e.serial_number != 0).then(|| e.serial_number.to_string())));
                IdentityFacts { name, serial, vendor: t.map(|t| t.vendor as u32).unwrap_or(0), model: t.map(|t| t.product as u32).unwrap_or(0), is_builtin: t.map(|t| t.builtin).unwrap_or(false) }
            })
            .collect();
        let ids = identity::stable_ids(&facts);
        self.gdi = ids.iter().cloned().zip(mons.iter().map(|(_, g)| g.clone())).collect();
        ids.into_iter().zip(facts).zip(mons).map(|((id, f), (hm, gdi))| (id, f, hm, gdi)).collect()
    }

    fn gdi_for(&mut self, id: &str) -> Result<String> {
        if !self.gdi.contains_key(id) {
            self.found();
        }
        self.gdi.get(id).cloned().ok_or_else(|| anyhow!("display {id:?} is not connected"))
    }
}

impl Backend for WinBackend {
    fn enumerate(&mut self) -> Vec<RawDisplay> {
        self.found()
            .into_iter()
            .map(|(id, f, hm, gdi)| {
                let mut dx = 0u32;
                let mut dy = 0u32;
                let dpi = (unsafe { GetDpiForMonitor(hm, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) } == 0 && dx > 0).then_some(dx);
                RawDisplay {
                    id,
                    name: f.name,
                    is_builtin: f.is_builtin,
                    vendor: f.vendor,
                    model: f.model,
                    current: current_devmode(&gdi).map(|d| mode_of(&d)),
                    modes: logic::normalize_modes(device_modes(&gdi).iter().map(mode_of).collect()),
                    size_mm: (0.0, 0.0),
                    dpi,
                }
            })
            .collect()
    }

    fn signature(&mut self) -> Vec<String> {
        let targets = display_config_targets();
        let mut v: Vec<String> = monitors().into_iter().map(|(_, gdi)| format!("{gdi}:{}", targets.get(&gdi).map(|t| t.device_path.as_str()).unwrap_or(""))).collect();
        v.sort();
        v
    }

    fn set_mode(&mut self, id: &str, target: Target, mode: &Mode, permanent: bool) -> Result<()> {
        if target == Target::Virtual {
            bail!("no virtual displays on Windows");
        }
        let gdi = self.gdi_for(id)?;
        let mut dm = device_modes(&gdi)
            .into_iter()
            .filter(|d| logic::same_mode(&mode_of(d), mode))
            .max_by_key(|d| d.dmBitsPerPel)
            .ok_or_else(|| anyhow!("{id} has no mode {}", mode.label()))?;
        dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY | DM_BITSPERPEL;
        let name = wide(&gdi);
        let r = unsafe { ChangeDisplaySettingsExW(name.as_ptr(), &dm, std::ptr::null_mut(), CDS_TEST, std::ptr::null()) };
        if r != DISP_CHANGE_SUCCESSFUL {
            bail!("{} rejected by the driver: {}", mode.label(), disp_change_name(r));
        }
        let flags = if permanent { CDS_UPDATEREGISTRY } else { 0 };
        let r = unsafe { ChangeDisplaySettingsExW(name.as_ptr(), &dm, std::ptr::null_mut(), flags, std::ptr::null()) };
        if r != DISP_CHANGE_SUCCESSFUL {
            bail!("switching {id} to {} failed: {}", mode.label(), disp_change_name(r));
        }
        Ok(())
    }

    fn hidpi_supported(&self) -> bool {
        false
    }

    fn enable_hidpi(&mut self, _: &str, _: &VirtualPlan) -> Result<()> {
        bail!("HiDPI virtual displays are macOS-only (Windows scales natively)")
    }

    fn disable_hidpi(&mut self, _: &str) -> Result<()> {
        Ok(())
    }

    fn virtual_modes(&mut self, _: &str) -> Option<(Option<Mode>, Vec<Mode>)> {
        None
    }

    fn repair_mirroring(&mut self, _: &[String]) {}
}
