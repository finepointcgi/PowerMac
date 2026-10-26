//! macOS hardware access:
//! * DDC/CI over the private `IOAVService` I2C API (Apple Silicon);
//! * native brightness through the private DisplayServices framework
//!   (built-in panel and Apple displays — including the LG UltraFine, which
//!   is controlled over USB rather than DDC), with CoreDisplay as fallback;
//! * gamma-table dimming via `CGSetDisplayTransferByTable`.
//!
//! Every private entry point is resolved with `dlsym`, so a missing one only
//! disables that path.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::CStr;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};

use super::ffi::*;
use crate::ddc::{self, DdcError};
use crate::engine::{Backend, RawDisplay, ReadError};
use crate::identity::{self, AvCandidate, CgFacts, IdentityFacts};
use crate::Rect;

/// Apple's PnP vendor number ("APP").
const APPLE_VENDOR: u32 = 0x0610;

/// Delay between writing a "Get VCP" request and reading the reply.
const READ_DELAY: Duration = Duration::from_millis(50);
/// Each write is sent this many times (some monitors drop single writes).
const WRITE_CYCLES: u32 = 2;
const WRITE_CYCLE_GAP: Duration = Duration::from_millis(10);

type IOAVServiceCreateWithServiceFn = unsafe extern "C" fn(CFAllocatorRef, IoObject) -> CFTypeRef;
type IOAVServiceReadI2CFn = unsafe extern "C" fn(CFTypeRef, u32, u32, *mut u8, u32) -> i32;
type IOAVServiceWriteI2CFn = unsafe extern "C" fn(CFTypeRef, u32, u32, *const u8, u32) -> i32;
type DSGetBrightnessFn = unsafe extern "C" fn(CGDirectDisplayID, *mut f32) -> i32;
type DSSetBrightnessFn = unsafe extern "C" fn(CGDirectDisplayID, f32) -> i32;
type DSCanChangeBrightnessFn = unsafe extern "C" fn(CGDirectDisplayID) -> bool;
type DSBrightnessChangedFn = unsafe extern "C" fn(CGDirectDisplayID, f64);
type CDGetUserBrightnessFn = unsafe extern "C" fn(CGDirectDisplayID) -> f64;
type CDSetUserBrightnessFn = unsafe extern "C" fn(CGDirectDisplayID, f64);
type CDCreateInfoDictionaryFn = unsafe extern "C" fn(CGDirectDisplayID) -> CFDictionaryRef;

/// Resolved private API.
struct Api {
    av_create: Option<IOAVServiceCreateWithServiceFn>,
    av_read: Option<IOAVServiceReadI2CFn>,
    av_write: Option<IOAVServiceWriteI2CFn>,
    ds_get: Option<DSGetBrightnessFn>,
    ds_set: Option<DSSetBrightnessFn>,
    ds_can_change: Option<DSCanChangeBrightnessFn>,
    ds_changed: Option<DSBrightnessChangedFn>,
    cd_get: Option<CDGetUserBrightnessFn>,
    cd_set: Option<CDSetUserBrightnessFn>,
    cd_info: Option<CDCreateInfoDictionaryFn>,
}

macro_rules! sym {
    ($path:expr, $name:literal) => {{
        let p = find_symbol($path, $name);
        if p.is_null() {
            None
        } else {
            #[allow(clippy::missing_transmute_annotations)]
            Some(unsafe { std::mem::transmute(p) })
        }
    }};
}

fn api() -> &'static Api {
    static API: OnceLock<Api> = OnceLock::new();
    API.get_or_init(|| {
        let iokit: Option<&CStr> = Some(c"/System/Library/Frameworks/IOKit.framework/IOKit");
        let ds: Option<&CStr> = Some(c"/System/Library/PrivateFrameworks/DisplayServices.framework/DisplayServices");
        let cd: Option<&CStr> = Some(c"/System/Library/Frameworks/CoreDisplay.framework/CoreDisplay");
        let api = Api {
            av_create: sym!(iokit, c"IOAVServiceCreateWithService"),
            av_read: sym!(iokit, c"IOAVServiceReadI2C"),
            av_write: sym!(iokit, c"IOAVServiceWriteI2C"),
            ds_get: sym!(ds, c"DisplayServicesGetBrightness"),
            ds_set: sym!(ds, c"DisplayServicesSetBrightness"),
            ds_can_change: sym!(ds, c"DisplayServicesCanChangeBrightness"),
            ds_changed: sym!(ds, c"DisplayServicesBrightnessChanged"),
            cd_get: sym!(cd, c"CoreDisplay_Display_GetUserBrightness"),
            cd_set: sym!(cd, c"CoreDisplay_Display_SetUserBrightness"),
            cd_info: sym!(cd, c"CoreDisplay_DisplayCreateInfoDictionary"),
        };
        log::debug!(
            "display-control: IOAVService {}, DisplayServices {}, CoreDisplay {}",
            api.av_create.is_some() && api.av_read.is_some() && api.av_write.is_some(),
            api.ds_get.is_some() && api.ds_set.is_some(),
            api.cd_info.is_some()
        );
        api
    })
}

/// An `IOAVServiceRef` (owned CF object).
struct AvService(Cf);

/// Original gamma tables of a display.
struct Gamma {
    red: Vec<f32>,
    green: Vec<f32>,
    blue: Vec<f32>,
}

struct Display {
    cg: CGDirectDisplayID,
    av: Option<AvService>,
}

#[derive(Default)]
pub(crate) struct MacBackend {
    displays: HashMap<String, Display>,
    gamma: HashMap<CGDirectDisplayID, Gamma>,
}

pub(crate) fn new_backend() -> Box<dyn Backend> {
    Box::new(MacBackend::default())
}

pub(crate) fn online_displays() -> Vec<CGDirectDisplayID> {
    let mut ids = [0u32; 32];
    let mut n = 0u32;
    if unsafe { CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut n) } != 0 {
        return Vec::new();
    }
    // Display Scaling's virtual HiDPI displays have no hardware to control
    // (the physical panel they mirror onto is listed on its own).
    ids[..n as usize]
        .iter()
        .copied()
        .filter(|&d| !crate::is_scaling_virtual_display(unsafe { CGDisplayVendorNumber(d) }, unsafe { CGDisplayModelNumber(d) }))
        .collect()
}

fn bounds(id: CGDirectDisplayID) -> Rect {
    let r = unsafe { CGDisplayBounds(id) };
    Rect { x: r.origin.x.round() as i32, y: r.origin.y.round() as i32, width: r.size.width.round() as i32, height: r.size.height.round() as i32 }
}

/// CoreDisplay's info dictionary for a display (owned).
fn info_dict(id: CGDirectDisplayID) -> Option<Cf> {
    let f = api().cd_info?;
    Cf::owned(unsafe { f(id) })
}

/// Everything we can learn about a CG display without IORegistry.
struct CgDisplay {
    id: CGDirectDisplayID,
    facts: CgFacts,
    name: String,
    serial: Option<String>,
    is_builtin: bool,
}

fn describe(id: CGDirectDisplayID) -> CgDisplay {
    let is_builtin = unsafe { CGDisplayIsBuiltin(id) } != 0;
    let size = unsafe { CGDisplayScreenSize(id) };
    let mut facts = CgFacts {
        vendor: unsafe { CGDisplayVendorNumber(id) },
        model: unsafe { CGDisplayModelNumber(id) },
        serial: unsafe { CGDisplaySerialNumber(id) },
        width_mm: size.width,
        height_mm: size.height,
        ..Default::default()
    };
    let mut name = None;
    let mut serial = None;
    if let Some(info) = info_dict(id) {
        let d = info.get();
        facts.location = cf_to_string(dict_get(d, "IODisplayLocation"));
        facts.week = cf_to_i64(dict_get(d, "DisplayWeekManufacture")).map(|v| v as u32);
        facts.year = cf_to_i64(dict_get(d, "DisplayYearManufacture")).map(|v| v as u32);
        let names = dict_get(d, "DisplayProductName");
        name = cf_to_string(dict_get(names, "en_US")).or_else(|| dict_values(names).into_iter().find_map(cf_to_string));
        if let Some(edid) = cf_to_bytes(dict_get(d, "IODisplayEDID")).and_then(|b| identity::parse_edid(&b)) {
            if name.is_none() {
                name = edid.name.clone();
            }
            serial = edid.serial_string.clone();
        }
        if serial.is_none() {
            serial = cf_to_string(dict_get(d, "kDisplaySerialString"))
                .or_else(|| cf_to_string(dict_get(d, "DisplaySerialString")))
                .or_else(|| cf_to_i64(dict_get(d, "DisplaySerialNumber")).filter(|&n| n != 0).map(|n| n.to_string()));
        }
    }
    if serial.is_none() && facts.serial != 0 {
        serial = Some(facts.serial.to_string());
    }
    let name = name.unwrap_or_else(|| {
        if is_builtin {
            "Built-in Display".to_string()
        } else {
            format!("{} Display {:04X}", identity::pnp_id(facts.vendor as u16), facts.model)
        }
    });
    CgDisplay { id, facts, name, serial, is_builtin }
}

/// Framebuffers with an external `DCPAVServiceProxy`, in registry order.
fn av_candidates() -> Vec<(AvCandidate, AvService)> {
    let api = api();
    let Some(create) = api.av_create else { return Vec::new() };
    if api.av_read.is_none() || api.av_write.is_none() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut iter: IoObject = 0;
    if unsafe { IORegistryCreateIterator(0, c"IOService".as_ptr(), kIORegistryIterateRecursively, &mut iter) } != 0 {
        return out;
    }
    let mut current: Option<AvCandidate> = None;
    loop {
        let entry = unsafe { IOIteratorNext(iter) };
        if entry == 0 {
            break;
        }
        let name = io_name(entry);
        if name == "AppleCLCD2" || name == "IOMobileFramebufferShim" {
            let mut c = AvCandidate { path: io_path(entry), ..Default::default() };
            c.edid_uuid = io_property(entry, "EDID UUID").and_then(|v| cf_to_string(v.get()));
            if let Some(attrs) = io_property(entry, "DisplayAttributes") {
                let pa = dict_get(attrs.get(), "ProductAttributes");
                c.product_name = cf_to_string(dict_get(pa, "ProductName"));
                c.serial_number = cf_to_i64(dict_get(pa, "SerialNumber")).map(|v| v as u32);
            }
            current = Some(c);
        } else if name == "DCPAVServiceProxy" {
            let loc = io_property(entry, "Location").and_then(|v| cf_to_string(v.get()));
            if loc.as_deref() == Some("External") {
                if let Some(c) = current.take() {
                    let svc = unsafe { create(kCFAllocatorDefault, entry) };
                    if let Some(svc) = Cf::owned(svc) {
                        out.push((c, AvService(svc)));
                    }
                }
            }
        }
        unsafe { IOObjectRelease(entry) };
    }
    unsafe { IOObjectRelease(iter) };
    out
}

fn native_can_change(id: CGDirectDisplayID, is_builtin: bool) -> bool {
    let api = api();
    if api.ds_set.is_none() && api.cd_set.is_none() {
        return false;
    }
    if let Some(can) = api.ds_can_change {
        return unsafe { can(id) };
    }
    // No CanChange: trust a successful read on the built-in panel only.
    is_builtin && native_get(id).is_some()
}

fn native_get(id: CGDirectDisplayID) -> Option<f32> {
    let api = api();
    if let Some(get) = api.ds_get {
        let mut v: f32 = -1.0;
        if unsafe { get(id, &mut v) } == 0 && (0.0..=1.0).contains(&v) {
            return Some(v);
        }
    }
    if let Some(get) = api.cd_get {
        let v = unsafe { get(id) };
        if (0.0..=1.0).contains(&v) {
            return Some(v as f32);
        }
    }
    None
}

impl AvService {
    fn write(&self, payload: &[u8]) -> Result<()> {
        let w = api().av_write.ok_or_else(|| anyhow!("IOAVServiceWriteI2C missing"))?;
        let r = unsafe { w(self.0.get(), ddc::DDC_I2C_ADDR as u32, ddc::HOST_ADDR as u32, payload.as_ptr(), payload.len() as u32) };
        if r != 0 {
            bail!("IOAVServiceWriteI2C failed: 0x{r:08X}");
        }
        Ok(())
    }

    fn read(&self, buf: &mut [u8]) -> Result<()> {
        let rd = api().av_read.ok_or_else(|| anyhow!("IOAVServiceReadI2C missing"))?;
        let r = unsafe { rd(self.0.get(), ddc::DDC_I2C_ADDR as u32, ddc::HOST_ADDR as u32, buf.as_mut_ptr(), buf.len() as u32) };
        if r != 0 {
            bail!("IOAVServiceReadI2C failed: 0x{r:08X}");
        }
        Ok(())
    }
}

impl MacBackend {
    fn cg(&self, id: &str) -> Result<CGDirectDisplayID> {
        self.displays.get(id).map(|d| d.cg).ok_or_else(|| anyhow!("unknown display {id:?}"))
    }
}

impl Backend for MacBackend {
    fn enumerate(&mut self) -> Vec<RawDisplay> {
        let cgs: Vec<CgDisplay> = online_displays()
            .into_iter()
            // Skip hardware-mirrored members that aren't the mirror master? No:
            // each mirrored monitor is a separate physical display with its own DDC.
            .map(describe)
            .collect();
        let externals: Vec<usize> = cgs.iter().enumerate().filter(|(_, d)| !d.is_builtin).map(|(i, _)| i).collect();
        let mut cands = av_candidates();
        let facts: Vec<CgFacts> = externals.iter().map(|&i| cgs[i].facts.clone()).collect();
        let cand_facts: Vec<AvCandidate> = cands.iter().map(|(c, _)| c.clone()).collect();
        let assignment = identity::assign(&facts, &cand_facts);
        let mut services: Vec<Option<AvService>> = cands.drain(..).map(|(_, s)| Some(s)).collect();
        let mut av_for: HashMap<usize, AvService> = HashMap::new();
        for (k, a) in assignment.into_iter().enumerate() {
            if let Some(j) = a {
                if let Some(s) = services[j].take() {
                    av_for.insert(externals[k], s);
                }
            }
        }
        let idfacts: Vec<IdentityFacts> = cgs
            .iter()
            .map(|d| IdentityFacts { name: d.name.clone(), serial: d.serial.clone(), vendor: d.facts.vendor, model: d.facts.model, is_builtin: d.is_builtin })
            .collect();
        let ids = identity::stable_ids(&idfacts);
        self.displays.clear();
        let mut out = Vec::new();
        for (i, (d, id)) in cgs.into_iter().zip(ids).enumerate() {
            let native = native_can_change(d.id, d.is_builtin);
            let mut av = av_for.remove(&i);
            // Apple displays with native brightness have no useful DDC.
            if native && d.facts.vendor == APPLE_VENDOR {
                av = None;
            }
            let gamma = unsafe { CGDisplayGammaTableCapacity(d.id) } > 0;
            out.push(RawDisplay { id: id.clone(), name: d.name.clone(), is_builtin: d.is_builtin, bounds: bounds(d.id), ddc: av.is_some(), native_brightness: native, gamma });
            self.displays.insert(id, Display { cg: d.id, av });
        }
        out
    }

    fn signature(&mut self) -> u64 {
        let mut h = DefaultHasher::new();
        for id in online_displays() {
            id.hash(&mut h);
            let b = bounds(id);
            (b.x, b.y, b.width, b.height).hash(&mut h);
        }
        h.finish()
    }

    fn ddc_read(&mut self, id: &str, vcp: u8) -> Result<(u16, u16), ReadError> {
        let av = self.displays.get(id).and_then(|d| d.av.as_ref()).ok_or(ReadError::Unsupported)?;
        av.write(&ddc::encode_get_vcp(vcp)).map_err(|e| ReadError::Transient(e.to_string()))?;
        std::thread::sleep(READ_DELAY);
        let mut buf = [0u8; ddc::GET_REPLY_LEN];
        av.read(&mut buf).map_err(|e| ReadError::Transient(e.to_string()))?;
        match ddc::decode_get_vcp_reply(&buf, vcp) {
            Ok(r) => Ok((r.current, r.max)),
            Err(DdcError::Unsupported) => Err(ReadError::Unsupported),
            Err(e) => Err(ReadError::Transient(format!("{e} ({buf:02X?})"))),
        }
    }

    fn ddc_write(&mut self, id: &str, vcp: u8, value: u16) -> Result<()> {
        let av = self.displays.get(id).and_then(|d| d.av.as_ref()).ok_or_else(|| anyhow!("{id:?} has no DDC channel"))?;
        let p = ddc::encode_set_vcp(vcp, value);
        let mut last = Ok(());
        for i in 0..WRITE_CYCLES {
            if i > 0 {
                std::thread::sleep(WRITE_CYCLE_GAP);
            }
            last = av.write(&p);
        }
        last
    }

    fn native_get(&mut self, id: &str) -> Option<f32> {
        native_get(self.cg(id).ok()?)
    }

    fn native_set(&mut self, id: &str, level: f32) -> Result<()> {
        let cg = self.cg(id)?;
        let level = level.clamp(0.0, 1.0);
        let api = api();
        if let Some(set) = api.ds_set {
            let r = unsafe { set(cg, level) };
            if r == 0 {
                if let Some(changed) = api.ds_changed {
                    unsafe { changed(cg, level as f64) };
                }
                return Ok(());
            }
            log::debug!("display-control: DisplayServicesSetBrightness({cg}) = {r}");
        }
        if let Some(set) = api.cd_set {
            unsafe { set(cg, level as f64) };
            return Ok(());
        }
        bail!("no native brightness API")
    }

    fn gamma_set(&mut self, id: &str, factor: f32) -> Result<()> {
        let cg = self.cg(id)?;
        if let std::collections::hash_map::Entry::Vacant(v) = self.gamma.entry(cg) {
            let cap = unsafe { CGDisplayGammaTableCapacity(cg) }.max(2);
            let (mut r, mut g, mut b) = (vec![0f32; cap as usize], vec![0f32; cap as usize], vec![0f32; cap as usize]);
            let mut n = 0u32;
            let err = unsafe { CGGetDisplayTransferByTable(cg, cap, r.as_mut_ptr(), g.as_mut_ptr(), b.as_mut_ptr(), &mut n) };
            if err != 0 || n == 0 {
                bail!("CGGetDisplayTransferByTable failed ({err})");
            }
            r.truncate(n as usize);
            g.truncate(n as usize);
            b.truncate(n as usize);
            v.insert(Gamma { red: r, green: g, blue: b });
        }
        let orig = &self.gamma[&cg];
        let f = factor.clamp(0.0, 1.0);
        let scale = |t: &[f32]| t.iter().map(|v| v * f).collect::<Vec<f32>>();
        let (r, g, b) = if f >= 1.0 { (orig.red.clone(), orig.green.clone(), orig.blue.clone()) } else { (scale(&orig.red), scale(&orig.green), scale(&orig.blue)) };
        let err = unsafe { CGSetDisplayTransferByTable(cg, r.len() as u32, r.as_ptr(), g.as_ptr(), b.as_ptr()) };
        if err != 0 {
            bail!("CGSetDisplayTransferByTable failed ({err})");
        }
        if f >= 1.0 {
            self.gamma.remove(&cg);
        }
        Ok(())
    }

    fn gamma_restore(&mut self) {
        let online = online_displays();
        for (cg, t) in self.gamma.drain() {
            if !online.contains(&cg) {
                continue;
            }
            let err = unsafe { CGSetDisplayTransferByTable(cg, t.red.len() as u32, t.red.as_ptr(), t.green.as_ptr(), t.blue.as_ptr()) };
            if err != 0 {
                log::warn!("display-control: restoring gamma of display {cg} failed ({err}); restoring ColorSync settings");
                unsafe { CGDisplayRestoreColorSyncSettings() };
            }
        }
    }
}

impl Drop for MacBackend {
    fn drop(&mut self) {
        self.gamma_restore();
    }
}

/// Diagnostics for the probe example: what IORegistry and CoreDisplay say.
pub fn debug_report() -> String {
    use std::fmt::Write;
    let api = api();
    let mut s = String::new();
    let _ = writeln!(
        s,
        "private API: IOAVService={} DisplayServices(get/set/canChange)={}/{}/{} CoreDisplay(info/get/set)={}/{}/{}",
        api.av_create.is_some() && api.av_read.is_some() && api.av_write.is_some(),
        api.ds_get.is_some(),
        api.ds_set.is_some(),
        api.ds_can_change.is_some(),
        api.cd_info.is_some(),
        api.cd_get.is_some(),
        api.cd_set.is_some()
    );
    for id in online_displays() {
        let d = describe(id);
        let _ = writeln!(
            s,
            "CG display {id}: builtin={} name={:?} serial={:?} vendor=0x{:04X} model=0x{:04X} cgSerial={} location={:?} week={:?} year={:?} size={:.0}x{:.0}mm native={} nativeLevel={:?}",
            d.is_builtin,
            d.name,
            d.serial,
            d.facts.vendor,
            d.facts.model,
            d.facts.serial,
            d.facts.location,
            d.facts.week,
            d.facts.year,
            d.facts.width_mm,
            d.facts.height_mm,
            native_can_change(id, d.is_builtin),
            native_get(id)
        );
    }
    // Raw registry walk (framebuffers and AV proxies, embedded included).
    let mut iter: IoObject = 0;
    if unsafe { IORegistryCreateIterator(0, c"IOService".as_ptr(), kIORegistryIterateRecursively, &mut iter) } == 0 {
        loop {
            let entry = unsafe { IOIteratorNext(iter) };
            if entry == 0 {
                break;
            }
            let name = io_name(entry);
            if name == "AppleCLCD2" || name == "IOMobileFramebufferShim" {
                let uuid = io_property(entry, "EDID UUID").and_then(|v| cf_to_string(v.get()));
                let _ = writeln!(s, "framebuffer {name}: path={:?} edidUUID={uuid:?}", io_path(entry));
            } else if name == "DCPAVServiceProxy" {
                let loc = io_property(entry, "Location").and_then(|v| cf_to_string(v.get()));
                let _ = writeln!(s, "  DCPAVServiceProxy Location={loc:?}");
            }
            unsafe { IOObjectRelease(entry) };
        }
        unsafe { IOObjectRelease(iter) };
    }
    for (c, _) in av_candidates() {
        let _ = writeln!(s, "external AV service: path={:?} edidUUID={:?} product={:?} serial={:?}", c.path, c.edid_uuid, c.product_name, c.serial_number);
    }
    s
}

/// Non-destructive self-test of the native and gamma write paths for every
/// online display: reads the native level and writes the *same* value back,
/// and sets the gamma table to its own current contents. Nothing visibly
/// changes. Returns one line per display.
pub fn debug_noop_writes() -> Vec<String> {
    let api = api();
    let mut out = Vec::new();
    for cg in online_displays() {
        let mut line = format!("display {cg}: ");
        match (native_get(cg), api.ds_set) {
            (Some(level), Some(set)) => {
                let r = unsafe { set(cg, level) };
                let after = native_get(cg);
                line += &format!("native {level:.4} -> set same ({r}) -> {after:?}; ");
            }
            (l, _) => line += &format!("native get {l:?} (no set attempted); "),
        }
        let cap = unsafe { CGDisplayGammaTableCapacity(cg) }.max(2);
        let (mut r, mut g, mut b) = (vec![0f32; cap as usize], vec![0f32; cap as usize], vec![0f32; cap as usize]);
        let mut n = 0u32;
        let get = unsafe { CGGetDisplayTransferByTable(cg, cap, r.as_mut_ptr(), g.as_mut_ptr(), b.as_mut_ptr(), &mut n) };
        if get == 0 && n > 0 {
            let set = unsafe { CGSetDisplayTransferByTable(cg, n, r.as_ptr(), g.as_ptr(), b.as_ptr()) };
            line += &format!("gamma table {n} entries (last {:.3}), identity set -> {set}", r[n as usize - 1]);
        } else {
            line += &format!("gamma get failed ({get})");
        }
        out.push(line);
    }
    out
}
