//! macOS backend: CoreGraphics display modes (including HiDPI and the
//! hidden duplicate low-resolution ones), mode switching in display
//! configuration transactions, and HiDPI through a mirrored
//! `CGVirtualDisplay`.

mod ffi;
mod virtual_display;

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use display_control::identity::{self, IdentityFacts};

use self::ffi::*;
use self::virtual_display::{Descriptor, VMode, VirtualDisplay};
use crate::engine::{Backend, RawDisplay};
use crate::logic::{self, Target, VirtualPlan};
use crate::Mode;

use crate::{VIRTUAL_PRODUCT, VIRTUAL_VENDOR};

pub(crate) fn hidpi_supported() -> bool {
    static AVAIL: OnceLock<bool> = OnceLock::new();
    *AVAIL.get_or_init(virtual_display::available)
}

pub(crate) fn new_backend() -> Box<dyn Backend> {
    Box::new(MacBackend::default())
}

fn is_ours(d: CGDirectDisplayID) -> bool {
    unsafe { CGDisplayVendorNumber(d) == VIRTUAL_VENDOR && CGDisplayModelNumber(d) == VIRTUAL_PRODUCT }
}

type CDCreateInfoDictionaryFn = unsafe extern "C" fn(CGDirectDisplayID) -> CFDictionaryRef;

fn cd_info_fn() -> Option<CDCreateInfoDictionaryFn> {
    static F: OnceLock<usize> = OnceLock::new();
    let p = *F.get_or_init(|| unsafe {
        let h = dlopen(c"/System/Library/Frameworks/CoreDisplay.framework/CoreDisplay".as_ptr(), RTLD_LAZY);
        if h.is_null() {
            return 0;
        }
        dlsym(h, c"CoreDisplay_DisplayCreateInfoDictionary".as_ptr()) as usize
    });
    (p != 0).then(|| unsafe { std::mem::transmute::<usize, CDCreateInfoDictionaryFn>(p) })
}

/// Name, serial, vendor, model (same rules as the Displays module, so the
/// stable ids match).
fn identity_facts(d: CGDirectDisplayID) -> IdentityFacts {
    let is_builtin = unsafe { CGDisplayIsBuiltin(d) } != 0;
    let vendor = unsafe { CGDisplayVendorNumber(d) };
    let model = unsafe { CGDisplayModelNumber(d) };
    let cg_serial = unsafe { CGDisplaySerialNumber(d) };
    let mut name = None;
    let mut serial = None;
    if let Some(info) = cd_info_fn().and_then(|f| Cf::owned(unsafe { f(d) })) {
        let i = info.get();
        let names = dict_get(i, "DisplayProductName");
        name = cf_to_string(dict_get(names, "en_US")).or_else(|| dict_values(names).into_iter().find_map(cf_to_string));
        if let Some(edid) = cf_to_bytes(dict_get(i, "IODisplayEDID")).and_then(|b| identity::parse_edid(&b)) {
            if name.is_none() {
                name = edid.name.clone();
            }
            serial = edid.serial_string.clone();
        }
        if serial.is_none() {
            serial = cf_to_string(dict_get(i, "kDisplaySerialString"))
                .or_else(|| cf_to_string(dict_get(i, "DisplaySerialString")))
                .or_else(|| cf_to_i64(dict_get(i, "DisplaySerialNumber")).filter(|&n| n != 0).map(|n| n.to_string()));
        }
    }
    if serial.is_none() && cg_serial != 0 {
        serial = Some(cg_serial.to_string());
    }
    let name = name.unwrap_or_else(|| if is_builtin { "Built-in Display".to_string() } else { format!("{} Display {:04X}", identity::pnp_id(vendor as u16), model) });
    IdentityFacts { name, serial, vendor, model, is_builtin }
}

fn mode_of(m: CGDisplayModeRef) -> Mode {
    unsafe {
        let (w, h, pw, ph) = (CGDisplayModeGetWidth(m) as u32, CGDisplayModeGetHeight(m) as u32, CGDisplayModeGetPixelWidth(m) as u32, CGDisplayModeGetPixelHeight(m) as u32);
        Mode { width: w, height: h, pixel_width: pw, pixel_height: ph, refresh: CGDisplayModeGetRefreshRate(m), hidpi: pw > w }
    }
}

/// An owned `CGDisplayModeRef`.
struct OwnedMode(CGDisplayModeRef);

impl Drop for OwnedMode {
    fn drop(&mut self) {
        unsafe { CGDisplayModeRelease(self.0) }
    }
}

/// Every mode, including the duplicate low-resolution (HiDPI) ones macOS
/// hides by default.
fn all_modes(d: CGDirectDisplayID) -> Vec<(Mode, bool, OwnedMode)> {
    let key: CFTypeRef = unsafe { kCGDisplayShowDuplicateLowResolutionModes };
    let val: CFTypeRef = unsafe { kCFBooleanTrue };
    let opts = Cf::owned(unsafe { CFDictionaryCreate(std::ptr::null(), &key, &val, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks) });
    let arr = Cf::owned(unsafe { CGDisplayCopyAllDisplayModes(d, opts.as_ref().map_or(std::ptr::null(), |o| o.get())) });
    let Some(arr) = arr else { return Vec::new() };
    let n = unsafe { CFArrayGetCount(arr.get()) };
    (0..n)
        .filter_map(|i| {
            let m = unsafe { CFArrayGetValueAtIndex(arr.get(), i) };
            if m.is_null() {
                return None;
            }
            let usable = unsafe { CGDisplayModeIsUsableForDesktopGUI(m) };
            Some((mode_of(m), usable, OwnedMode(unsafe { CGDisplayModeRetain(m) })))
        })
        .collect()
}

fn usable_modes(d: CGDirectDisplayID) -> Vec<Mode> {
    logic::normalize_modes(all_modes(d).into_iter().filter(|(_, usable, _)| *usable).map(|(m, _, _)| m).collect())
}

fn current_mode(d: CGDirectDisplayID) -> Option<Mode> {
    let m = unsafe { CGDisplayCopyDisplayMode(d) };
    (!m.is_null()).then(|| {
        let o = OwnedMode(m);
        mode_of(o.0)
    })
}

/// Switch `d` to `mode` in one configuration transaction.
fn set_display_mode(d: CGDirectDisplayID, mode: &Mode, option: u32) -> Result<()> {
    let modes = all_modes(d);
    let (_, _, m) = modes
        .iter()
        .filter(|(m, _, _)| logic::same_mode(m, mode))
        .max_by_key(|(_, usable, _)| *usable)
        .ok_or_else(|| anyhow!("display {d} has no mode {}", mode.label()))?;
    configure(option, |cfg| unsafe { CGConfigureDisplayWithDisplayMode(cfg, d, m.0, std::ptr::null()) })
}

fn mirror(d: CGDirectDisplayID, master: CGDirectDisplayID) -> Result<()> {
    // For the app only: macOS undoes it by itself if PowerMac exits or crashes.
    configure(kCGConfigureForAppOnly, |cfg| unsafe { CGConfigureDisplayMirrorOfDisplay(cfg, d, master) })
}

fn serial_for(id: &str) -> u32 {
    let mut h = DefaultHasher::new();
    id.hash(&mut h);
    (h.finish() as u32).max(1)
}

struct Virtual {
    display: VirtualDisplay,
    physical: CGDirectDisplayID,
}

#[derive(Default)]
struct MacBackend {
    /// Stable id → CG display id, from the last enumeration.
    ids: HashMap<String, CGDirectDisplayID>,
    virtuals: HashMap<String, Virtual>,
}

impl MacBackend {
    fn physical(&self) -> Vec<CGDirectDisplayID> {
        online_displays().into_iter().filter(|&d| !is_ours(d)).collect()
    }

    fn refresh_ids(&mut self) -> Vec<(CGDirectDisplayID, String, IdentityFacts)> {
        let ds = self.physical();
        let facts: Vec<IdentityFacts> = ds.iter().map(|&d| identity_facts(d)).collect();
        let ids = identity::stable_ids(&facts);
        self.ids = ids.iter().cloned().zip(ds.iter().copied()).collect();
        ds.into_iter().zip(ids).zip(facts).map(|((d, id), f)| (d, id, f)).collect()
    }

    fn cg_id(&mut self, id: &str) -> Result<CGDirectDisplayID> {
        if let Some(&d) = self.ids.get(id) {
            if unsafe { CGDisplayIsOnline(d) } != 0 {
                return Ok(d);
            }
        }
        self.refresh_ids();
        self.ids.get(id).copied().ok_or_else(|| anyhow!("display {id:?} is not connected"))
    }

    fn unmirror(d: CGDirectDisplayID) -> Result<()> {
        if unsafe { CGDisplayMirrorsDisplay(d) } != kCGNullDirectDisplay {
            configure(kCGConfigureForAppOnly, |cfg| unsafe { CGConfigureDisplayMirrorOfDisplay(cfg, d, kCGNullDirectDisplay) })?;
        }
        Ok(())
    }
}

impl Backend for MacBackend {
    fn enumerate(&mut self) -> Vec<RawDisplay> {
        self.refresh_ids()
            .into_iter()
            .map(|(d, id, f)| {
                let size = unsafe { CGDisplayScreenSize(d) };
                RawDisplay {
                    id,
                    name: f.name,
                    is_builtin: f.is_builtin,
                    vendor: f.vendor,
                    model: f.model,
                    current: current_mode(d),
                    modes: usable_modes(d),
                    size_mm: (size.width, size.height),
                    dpi: None,
                }
            })
            .collect()
    }

    fn signature(&mut self) -> Vec<String> {
        self.physical().into_iter().map(|d| unsafe { format!("{d}:{:x}:{:x}:{:x}", CGDisplayVendorNumber(d), CGDisplayModelNumber(d), CGDisplaySerialNumber(d)) }).collect()
    }

    fn set_mode(&mut self, id: &str, target: Target, mode: &Mode, permanent: bool) -> Result<()> {
        let (d, option) = match target {
            Target::Physical => (self.cg_id(id)?, if permanent { kCGConfigurePermanently } else { kCGConfigureForSession }),
            Target::Virtual => (self.virtuals.get(id).ok_or_else(|| anyhow!("{id} has no HiDPI display"))?.display.display_id, if permanent { kCGConfigurePermanently } else { kCGConfigureForAppOnly }),
        };
        set_display_mode(d, mode, option)
    }

    fn hidpi_supported(&self) -> bool {
        hidpi_supported()
    }

    fn enable_hidpi(&mut self, id: &str, plan: &VirtualPlan) -> Result<()> {
        let phys = self.cg_id(id)?;
        if unsafe { CGDisplayIsBuiltin(phys) } != 0 {
            bail!("{id} is a built-in display");
        }
        let modes: Vec<VMode> = plan.modes_target_first().iter().map(|r| VMode { width: r.width, height: r.height, refresh: plan.refresh }).collect();
        let existing = self.virtuals.get(id).filter(|v| v.physical == phys && online_displays().contains(&v.display.display_id));
        if let Some(v) = existing {
            v.display.apply_modes(&modes, true)?;
        } else {
            self.virtuals.remove(id);
            let name = format!("{} (HiDPI)", identity_facts(phys).name);
            let display = VirtualDisplay::create(&Descriptor {
                name: &name,
                vendor: VIRTUAL_VENDOR,
                product: VIRTUAL_PRODUCT,
                serial: serial_for(id),
                max_pixels: (plan.max_pixels.width, plan.max_pixels.height),
                size_mm: plan.size_mm,
            })?;
            display.apply_modes(&modes, true)?;
            if !display.wait_online(Duration::from_secs(3)) {
                bail!("virtual display {} did not come online", display.display_id);
            }
            // Its first mode (the target, HiDPI) is active from the start, so
            // this only matters for switching modes later.
            if !display.wait_modes(Duration::from_secs(3)) {
                log::warn!("display-scaling: modes of virtual display {} are not visible to this process yet", display.display_id);
            }
            log::info!("display-scaling: virtual display {} for {id} ({} modes)", display.display_id, modes.len());
            self.virtuals.insert(id.to_string(), Virtual { display, physical: phys });
        }
        let v = &self.virtuals[id];
        if unsafe { CGDisplayMirrorsDisplay(phys) } != v.display.display_id {
            mirror(phys, v.display.display_id)?;
        }
        Ok(())
    }

    fn disable_hidpi(&mut self, id: &str) -> Result<()> {
        let r = match self.virtuals.get(id) {
            Some(v) if unsafe { CGDisplayIsOnline(v.physical) } != 0 && unsafe { CGDisplayMirrorsDisplay(v.physical) } == v.display.display_id => Self::unmirror(v.physical),
            _ => Ok(()),
        };
        // Dropping the object removes the virtual display (and any mirror onto it).
        self.virtuals.remove(id);
        r
    }

    fn virtual_modes(&mut self, id: &str) -> Option<(Option<Mode>, Vec<Mode>)> {
        let d = self.virtuals.get(id)?.display.display_id;
        Some((current_mode(d), usable_modes(d)))
    }

    fn repair_mirroring(&mut self, ids: &[String]) {
        let ours: Vec<CGDirectDisplayID> = self.virtuals.values().map(|v| v.display.display_id).collect();
        let online = online_displays();
        for (d, id, _) in self.refresh_ids() {
            let master = unsafe { CGDisplayMirrorsDisplay(d) };
            if master == kCGNullDirectDisplay || ours.contains(&master) {
                continue;
            }
            // Mirroring a display that's gone, or a leftover virtual display of ours.
            if ids.contains(&id) && (!online.contains(&master) || is_ours(master)) {
                log::info!("display-scaling: un-mirroring {id} (left over from a previous run)");
                if let Err(e) = Self::unmirror(d) {
                    log::warn!("display-scaling: un-mirroring {id}: {e:#}");
                }
            }
        }
    }
}

impl Drop for MacBackend {
    fn drop(&mut self) {
        let ids: Vec<String> = self.virtuals.keys().cloned().collect();
        for id in ids {
            let _ = self.disable_hidpi(&id);
        }
    }
}

/// Live check of the private API without touching any real display:
/// create a virtual display with the HiDPI plan for a `panel_w` x
/// `panel_h` panel, read back its modes, destroy it. No mirroring, no mode
/// changes. The display starts in `target` (logical, HiDPI) and is kept
/// for `hold` before it is destroyed. Returns a human-readable report.
pub fn virtual_display_self_test(panel_w: u32, panel_h: u32, target: Option<crate::Resolution>, hold: Duration) -> Result<String> {
    use std::fmt::Write as _;
    let mut out = String::new();
    for s in virtual_display::signatures() {
        let _ = writeln!(out, "{s}");
    }
    let plan = logic::plan_virtual(crate::Resolution { width: panel_w, height: panel_h }, (597.0, 336.0), 60.0, &crate::DisplayConfig { target_logical_resolution: target, ..Default::default() });
    let before = online_displays();
    let d = VirtualDisplay::create(&Descriptor {
        name: "PowerMac HiDPI self-test",
        vendor: VIRTUAL_VENDOR,
        product: VIRTUAL_PRODUCT,
        serial: 0x7E57,
        max_pixels: (plan.max_pixels.width, plan.max_pixels.height),
        size_mm: plan.size_mm,
    })?;
    let modes: Vec<VMode> = plan.modes_target_first().iter().map(|r| VMode { width: r.width, height: r.height, refresh: 60.0 }).collect();
    let applied = d.apply_modes(&modes, true);
    let t0 = std::time::Instant::now();
    let online = d.wait_online(Duration::from_secs(3));
    let with_modes = online && d.wait_modes(Duration::from_secs(3));
    let _ = writeln!(out, "online {online} / modes visible {with_modes} after {:?}", t0.elapsed());
    let id = d.display_id;
    let _ = writeln!(out, "virtual display {id}: applySettings {:?}, online {online}, ours {}", applied.as_ref().map_err(|e| e.to_string()), is_ours(id));
    if with_modes {
        let _ = writeln!(out, "current: {:?}", current_mode(id).map(|m| m.label()));
        let _ = writeln!(out, "mirrors: {}", unsafe { CGDisplayMirrorsDisplay(id) });
        for m in usable_modes(id) {
            let _ = writeln!(out, "  {} [{}x{} px]", m.label(), m.pixel_width, m.pixel_height);
        }
    }
    virtual_display::pump(hold);
    drop(d);
    let gone = (0..120).any(|_| {
        std::thread::sleep(Duration::from_millis(25));
        !online_displays().contains(&id)
    });
    let _ = writeln!(out, "destroyed: {gone}; displays before {before:?}, after {:?}", online_displays());
    applied?;
    if !online {
        bail!("{out}virtual display never came online");
    }
    Ok(out)
}
