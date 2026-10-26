//! Pure logic (unit-tested on every platform): mode selection, HiDPI mode
//! generation for a panel, config ↔ display id matching, and the
//! "revert unless confirmed" timer.

use std::collections::BTreeMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::{DisplayConfig, Mode, ModeSpec, Resolution, Token};

/// Largest backing store (in pixels, either side) offered for a virtual
/// HiDPI display. macOS accepts larger, but the GPU cost grows fast and
/// 8K-wide framebuffers are where WindowServer starts refusing modes.
pub const MAX_BACKING: u32 = 8192;

/// Smallest logical width offered by the generator.
const MIN_LOGICAL_WIDTH: u32 = 1024;

/// Common logical widths; the generator keeps the ones that fit the panel.
const STANDARD_WIDTHS: &[u32] = &[
    1024, 1152, 1280, 1344, 1440, 1600, 1680, 1728, 1792, 1920, 2048, 2176, 2304, 2560, 2880, 3008, 3200, 3360, 3440, 3840, 4096, 5120,
];

/// Two modes are "the same" if they have the same logical and pixel size,
/// the same HiDPI flag and refresh rates within half a hertz.
pub fn same_mode(a: &Mode, b: &Mode) -> bool {
    a.width == b.width && a.height == b.height && a.pixel_width == b.pixel_width && a.pixel_height == b.pixel_height && a.hidpi == b.hidpi && (a.refresh - b.refresh).abs() < 0.5
}

/// Drop duplicates (CoreGraphics lists the same mode several times with
/// different IO flags) and sort: logical width, height, HiDPI first, refresh
/// (highest first).
pub fn normalize_modes(modes: Vec<Mode>) -> Vec<Mode> {
    let mut out: Vec<Mode> = Vec::with_capacity(modes.len());
    for m in modes {
        if m.width == 0 || m.height == 0 {
            continue;
        }
        if !out.iter().any(|o| same_mode(o, &m)) {
            out.push(m);
        }
    }
    out.sort_by(|a, b| {
        b.width
            .cmp(&a.width)
            .then(b.height.cmp(&a.height))
            .then(b.hidpi.cmp(&a.hidpi))
            .then(b.refresh.partial_cmp(&a.refresh).unwrap_or(std::cmp::Ordering::Equal))
    });
    out
}

/// Pick the mode matching `spec` from `modes`: same logical size; the HiDPI
/// flag if given (otherwise HiDPI is preferred); the refresh rate closest to
/// the requested one (otherwise the highest).
pub fn select_mode(modes: &[Mode], spec: &ModeSpec) -> Option<Mode> {
    let mut c: Vec<&Mode> = modes.iter().filter(|m| m.width == spec.width && m.height == spec.height).collect();
    if let Some(h) = spec.hidpi {
        c.retain(|m| m.hidpi == h);
    } else if c.iter().any(|m| m.hidpi) {
        c.retain(|m| m.hidpi);
    }
    if let (Some(pw), Some(ph)) = (spec.pixel_width, spec.pixel_height) {
        if c.iter().any(|m| m.pixel_width == pw && m.pixel_height == ph) {
            c.retain(|m| m.pixel_width == pw && m.pixel_height == ph);
        }
    }
    match spec.refresh {
        Some(r) if r > 0.0 => c.into_iter().min_by(|a, b| (a.refresh - r).abs().partial_cmp(&(b.refresh - r).abs()).unwrap_or(std::cmp::Ordering::Equal)).copied(),
        _ => c.into_iter().max_by(|a, b| a.refresh.partial_cmp(&b.refresh).unwrap_or(std::cmp::Ordering::Equal)).copied(),
    }
}

/// The panel's native resolution: the largest pixel size among its 1x
/// modes (HiDPI modes may be supersampled beyond the panel), else among all.
pub fn native_resolution(modes: &[Mode]) -> Option<Resolution> {
    let plain: Vec<&Mode> = modes.iter().filter(|m| !m.hidpi).collect();
    let pool: Vec<&Mode> = if plain.is_empty() { modes.iter().collect() } else { plain };
    pool.into_iter().max_by_key(|m| (m.pixel_width as u64) * (m.pixel_height as u64)).map(|m| Resolution { width: m.pixel_width, height: m.pixel_height })
}

fn even(v: f64) -> u32 {
    let r = v.round() as u32;
    r + (r & 1)
}

/// Logical resolutions to offer as HiDPI (2x) modes on a panel of
/// `panel` pixels: standard widths with the panel's aspect ratio from half
/// the panel width (or 1024) up to the native width, plus the user's custom
/// ones. Every entry has a backing store (2x) within [`MAX_BACKING`].
/// Sorted by width, ascending, without duplicates.
pub fn hidpi_logical_resolutions(panel: Resolution, custom: &[Resolution]) -> Vec<Resolution> {
    let mut out: Vec<Resolution> = Vec::new();
    let fits = |r: &Resolution| r.width >= 320 && r.height >= 200 && r.width * 2 <= MAX_BACKING && r.height * 2 <= MAX_BACKING;
    let push = |r: Resolution, out: &mut Vec<Resolution>| {
        if fits(&r) && !out.contains(&r) {
            out.push(r);
        }
    };
    if panel.width > 0 && panel.height > 0 {
        let aspect = panel.height as f64 / panel.width as f64;
        let lo = (panel.width / 2).max(MIN_LOGICAL_WIDTH.min(panel.width));
        let mut widths: Vec<u32> = STANDARD_WIDTHS.iter().copied().filter(|&w| w >= lo && w <= panel.width).collect();
        widths.push(panel.width);
        if panel.width / 2 >= MIN_LOGICAL_WIDTH {
            widths.push(panel.width / 2);
        }
        for w in widths {
            let h = if w == panel.width { panel.height } else if w * 2 == panel.width { panel.height / 2 } else { even(w as f64 * aspect) };
            push(Resolution { width: w, height: h }, &mut out);
        }
    }
    for c in custom {
        push(*c, &mut out);
    }
    out.sort_by_key(|r| (r.width, r.height));
    out
}

/// What the virtual display backing a physical one should offer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualPlan {
    /// Logical sizes given to `CGVirtualDisplayMode`. With `hiDPI` set,
    /// macOS offers each as a HiDPI mode (2x backing store, as long as it
    /// fits `max_pixels`) and as a plain 1x mode.
    pub logical: Vec<Resolution>,
    /// 2x the largest logical size.
    pub max_pixels: Resolution,
    pub refresh: f64,
    /// Mode to switch the virtual display to (HiDPI, logical size).
    pub target: Resolution,
    /// Physical size, so macOS computes the right DPI.
    pub size_mm: (f64, f64),
}

impl VirtualPlan {
    /// The modes in the order given to the virtual display: the target
    /// first (a new virtual display starts in its first mode, HiDPI).
    pub fn modes_target_first(&self) -> Vec<Resolution> {
        let mut v = vec![self.target];
        v.extend(self.logical.iter().copied().filter(|r| *r != self.target));
        v
    }
}

/// Plan the virtual display for a physical panel of `panel` pixels.
pub fn plan_virtual(panel: Resolution, size_mm: (f64, f64), refresh: f64, cfg: &DisplayConfig) -> VirtualPlan {
    let mut logical = hidpi_logical_resolutions(panel, &cfg.custom_resolutions);
    logical.reverse(); // largest first: the first mode is the display's default
    let max_pixels = Resolution {
        width: logical.iter().map(|r| r.width * 2).max().unwrap_or(panel.width),
        height: logical.iter().map(|r| r.height * 2).max().unwrap_or(panel.height),
    };
    let target = cfg
        .preferred_mode
        .as_ref()
        .map(|m| Resolution { width: m.width, height: m.height })
        .or(cfg.target_logical_resolution)
        .filter(|t| logical.contains(t))
        .or_else(|| logical.iter().copied().find(|r| *r == panel))
        .or_else(|| logical.first().copied())
        .unwrap_or(panel);
    VirtualPlan { logical, max_pixels, refresh: if refresh > 1.0 { refresh } else { 60.0 }, target, size_mm }
}

/// Strip a `"~2"` duplicate suffix from a stable id.
pub fn base_id(id: &str) -> &str {
    match id.rfind('~') {
        Some(i) if id[i + 1..].chars().all(|c| c.is_ascii_digit()) && i + 1 < id.len() => &id[..i],
        _ => id,
    }
}

/// The config entry for display `id`: an exact match, else one keyed by
/// the same id without its duplicate suffix, else (for keys of the form
/// `"v<vendor>m<model>"`, hex) one matching vendor/model.
pub fn match_config<'a>(id: &str, vendor: u32, model: u32, displays: &'a BTreeMap<String, DisplayConfig>) -> Option<(&'a String, &'a DisplayConfig)> {
    if let Some(kv) = displays.get_key_value(id) {
        return Some(kv);
    }
    let base = base_id(id);
    if let Some(kv) = displays.get_key_value(base) {
        return Some(kv);
    }
    let vm = format!("v{vendor:04x}m{model:04x}");
    displays.iter().find(|(k, _)| k.eq_ignore_ascii_case(&vm))
}

/// Which display a pending change applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Target {
    /// The display itself.
    Physical,
    /// The HiDPI virtual display it mirrors.
    Virtual,
}

/// An unconfirmed mode change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub token: Token,
    pub display_id: String,
    pub target: Target,
    /// Mode to go back to.
    pub previous: Mode,
    pub applied: Mode,
    pub permanent: bool,
    #[serde(skip, default = "Instant::now")]
    pub deadline: Instant,
}

/// "Revert in N seconds unless confirmed". One pending change per display
/// (and target): applying again while one is pending keeps the original
/// `previous` mode, so a revert goes back to what was there before the
/// first unconfirmed change.
#[derive(Debug, Default)]
pub struct RevertTimer {
    next: u64,
    pending: Vec<Pending>,
}

impl RevertTimer {
    pub fn arm(&mut self, display_id: &str, target: Target, previous: Mode, applied: Mode, permanent: bool, deadline: Instant) -> Token {
        self.next += 1;
        let token = Token(self.next);
        let previous = match self.pending.iter().position(|p| p.display_id == display_id && p.target == target) {
            Some(i) => self.pending.remove(i).previous,
            None => previous,
        };
        self.pending.push(Pending { token, display_id: display_id.to_string(), target, previous, applied, permanent, deadline });
        token
    }

    /// Accept the change; returns it (so the caller can make it permanent).
    pub fn confirm(&mut self, token: Token) -> Option<Pending> {
        let i = self.pending.iter().position(|p| p.token == token)?;
        Some(self.pending.remove(i))
    }

    /// Changes whose deadline has passed (removed; the caller reverts them).
    pub fn expired(&mut self, now: Instant) -> Vec<Pending> {
        let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|p| p.deadline <= now);
        self.pending = keep;
        gone
    }

    /// Everything still pending (removed; used on stop).
    pub fn drain(&mut self) -> Vec<Pending> {
        std::mem::take(&mut self.pending)
    }

    /// Drop pending changes for displays that went away.
    pub fn forget_display(&mut self, display_id: &str) {
        self.pending.retain(|p| p.display_id != display_id);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.iter().map(|p| p.deadline).min()
    }

    pub fn pending(&self) -> &[Pending] {
        &self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn m(w: u32, h: u32, pw: u32, ph: u32, r: f64) -> Mode {
        Mode { width: w, height: h, pixel_width: pw, pixel_height: ph, refresh: r, hidpi: pw > w }
    }
    fn res(w: u32, h: u32) -> Resolution {
        Resolution { width: w, height: h }
    }

    #[test]
    fn normalize_dedupes_and_sorts() {
        let v = normalize_modes(vec![m(1920, 1080, 1920, 1080, 60.0), m(1920, 1080, 3840, 2160, 60.0), m(1920, 1080, 1920, 1080, 60.2), m(2560, 1440, 2560, 1440, 59.95), m(0, 0, 0, 0, 0.0), m(1920, 1080, 1920, 1080, 144.0)]);
        assert_eq!(v.len(), 4);
        assert_eq!((v[0].width, v[0].hidpi), (2560, false));
        assert!(v[1].hidpi);
        assert_eq!(v[2].refresh, 144.0);
    }

    #[test]
    fn select_prefers_hidpi_and_refresh() {
        let modes = vec![m(1920, 1080, 1920, 1080, 60.0), m(1920, 1080, 3840, 2160, 60.0), m(1920, 1080, 1920, 1080, 144.0), m(2560, 1440, 2560, 1440, 60.0)];
        let s = |hidpi, refresh| ModeSpec { width: 1920, height: 1080, hidpi, refresh, pixel_width: None, pixel_height: None };
        assert!(select_mode(&modes, &s(None, None)).unwrap().hidpi);
        let lo = select_mode(&modes, &s(Some(false), None)).unwrap();
        assert_eq!((lo.hidpi, lo.refresh), (false, 144.0));
        assert_eq!(select_mode(&modes, &s(Some(false), Some(59.94))).unwrap().refresh, 60.0);
        assert!(select_mode(&modes, &ModeSpec { width: 1280, height: 720, ..s(None, None) }).is_none());
        let exact = ModeSpec { pixel_width: Some(1920), pixel_height: Some(1080), ..s(None, None) };
        // hidpi filter wins before pixel sizes (none match → falls back to the hidpi one)
        assert!(select_mode(&modes, &exact).unwrap().hidpi);
    }

    #[test]
    fn hidpi_for_1440p() {
        let v = hidpi_logical_resolutions(res(2560, 1440), &[]);
        assert_eq!(v, vec![res(1280, 720), res(1344, 756), res(1440, 810), res(1600, 900), res(1680, 945 + 1), res(1728, 972), res(1792, 1008), res(1920, 1080), res(2048, 1152), res(2176, 1224), res(2304, 1296), res(2560, 1440)]);
        for r in &v {
            assert!(r.width * 2 <= MAX_BACKING && r.height % 2 == 0, "{r:?}");
        }
    }

    #[test]
    fn hidpi_for_other_panels() {
        let fhd = hidpi_logical_resolutions(res(1920, 1080), &[]);
        assert_eq!(fhd.first(), Some(&res(1024, 576)));
        assert_eq!(fhd.last(), Some(&res(1920, 1080)));
        let uw = hidpi_logical_resolutions(res(3440, 1440), &[]);
        assert!(uw.contains(&res(3440, 1440)) && uw.contains(&res(1720, 720)));
        assert!(uw.iter().all(|r| (r.width as f64 / r.height as f64 - 3440.0 / 1440.0).abs() < 0.01));
        let k4 = hidpi_logical_resolutions(res(3840, 2160), &[]);
        assert_eq!(k4.last(), Some(&res(3840, 2160)));
        assert!(k4.contains(&res(1920, 1080)) && k4.contains(&res(3008, 1692)));
        // 5K: 5120x2880 HiDPI would need a 10240-wide backing store
        let k5 = hidpi_logical_resolutions(res(5120, 2880), &[]);
        assert_eq!(k5.last(), Some(&res(4096, 2304)));
        assert!(hidpi_logical_resolutions(res(0, 0), &[]).is_empty());
    }

    #[test]
    fn custom_resolutions_are_added() {
        let v = hidpi_logical_resolutions(res(2560, 1440), &[res(2200, 1238), res(1920, 1080), res(5000, 3000)]);
        assert!(v.contains(&res(2200, 1238)));
        assert_eq!(v.iter().filter(|r| **r == res(1920, 1080)).count(), 1);
        assert!(!v.contains(&res(5000, 3000)), "backing over the limit");
    }

    #[test]
    fn plan_for_1440p() {
        let cfg = DisplayConfig { hidpi_enabled: true, target_logical_resolution: Some(res(1920, 1080)), custom_resolutions: vec![res(2200, 1238)], ..Default::default() };
        let p = plan_virtual(res(2560, 1440), (597.0, 336.0), 0.0, &cfg);
        assert_eq!(p.target, res(1920, 1080));
        assert_eq!(p.max_pixels, res(5120, 2880));
        assert!(p.logical.contains(&res(1920, 1080)) && p.logical.contains(&res(2048, 1152)) && p.logical.contains(&res(2304, 1296)) && p.logical.contains(&res(2200, 1238)));
        assert_eq!(p.logical[0], res(2560, 1440));
        assert_eq!(&p.modes_target_first()[..2], &[res(1920, 1080), res(2560, 1440)]);
        assert_eq!(p.modes_target_first().len(), p.logical.len());
        assert_eq!(p.refresh, 60.0);
        // default target: the panel's own size, rendered at 2x
        let p = plan_virtual(res(2560, 1440), (597.0, 336.0), 75.0, &DisplayConfig { hidpi_enabled: true, ..Default::default() });
        assert_eq!((p.target, p.refresh), (res(2560, 1440), 75.0));
        // an unusable target falls back
        let p = plan_virtual(res(2560, 1440), (0.0, 0.0), 60.0, &DisplayConfig { target_logical_resolution: Some(res(999, 1)), ..Default::default() });
        assert_eq!(p.target, res(2560, 1440));
        // preferred mode wins over the target
        let p = plan_virtual(
            res(2560, 1440),
            (0.0, 0.0),
            60.0,
            &DisplayConfig { target_logical_resolution: Some(res(1920, 1080)), preferred_mode: Some(ModeSpec { width: 2048, height: 1152, hidpi: Some(true), refresh: None, pixel_width: None, pixel_height: None }), ..Default::default() },
        );
        assert_eq!(p.target, res(2048, 1152));
    }

    #[test]
    fn id_matching() {
        let mut d = BTreeMap::new();
        d.insert("LG ULTRAGEAR#ABC123".to_string(), DisplayConfig { hidpi_enabled: true, ..Default::default() });
        d.insert("v1e6dm5b7f".to_string(), DisplayConfig::default());
        assert_eq!(match_config("LG ULTRAGEAR#ABC123", 0, 0, &d).unwrap().0, "LG ULTRAGEAR#ABC123");
        assert_eq!(match_config("LG ULTRAGEAR#ABC123~2", 0, 0, &d).unwrap().0, "LG ULTRAGEAR#ABC123");
        assert_eq!(match_config("Other#v1e6dm5b7f", 0x1e6d, 0x5b7f, &d).unwrap().0, "v1e6dm5b7f");
        assert!(match_config("Other#1", 1, 2, &d).is_none());
        assert_eq!(base_id("a~b"), "a~b");
        assert_eq!(base_id("a~"), "a~");
        assert_eq!(base_id("a~12"), "a");
    }

    #[test]
    fn native_ignores_supersampled_hidpi() {
        let v = [m(1512, 982, 3024, 1964, 60.0), m(1800, 1169, 3600, 2338, 60.0), m(3024, 1964, 3024, 1964, 60.0)];
        assert_eq!(native_resolution(&v), Some(res(3024, 1964)));
        assert_eq!(native_resolution(&v[..2]), Some(res(3600, 2338)));
        assert_eq!(native_resolution(&[]), None);
    }

    #[test]
    fn revert_timer() {
        let t0 = Instant::now();
        let mut t = RevertTimer::default();
        let a = m(2560, 1440, 2560, 1440, 60.0);
        let b = m(1920, 1080, 1920, 1080, 60.0);
        let c = m(1280, 720, 1280, 720, 60.0);
        let k1 = t.arm("d1", Target::Physical, a, b, false, t0 + Duration::from_secs(15));
        assert_eq!(t.next_deadline(), Some(t0 + Duration::from_secs(15)));
        // second change on the same display before confirming: revert goes to `a`
        let k2 = t.arm("d1", Target::Physical, b, c, true, t0 + Duration::from_secs(5) + Duration::from_secs(15));
        assert_ne!(k1, k2);
        assert!(t.confirm(k1).is_none(), "superseded token");
        assert_eq!(t.pending().len(), 1);
        let k3 = t.arm("d2", Target::Virtual, a, c, false, t0 + Duration::from_secs(1));
        assert!(t.expired(t0 + Duration::from_millis(999)).is_empty());
        let e = t.expired(t0 + Duration::from_secs(1));
        assert_eq!((e.len(), e[0].token), (1, k3));
        assert!(t.confirm(k3).is_none(), "already reverted");
        let e = t.expired(t0 + Duration::from_secs(19));
        assert!(e.is_empty());
        let p = t.confirm(k2).unwrap();
        assert_eq!((p.previous, p.applied, p.permanent), (a, c, true));
        assert_eq!(t.next_deadline(), None);
        t.arm("d3", Target::Physical, a, b, false, t0 + Duration::from_secs(15));
        t.forget_display("d3");
        assert!(t.drain().is_empty());
    }
}
