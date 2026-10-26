//! Display identity: EDID parsing, stable ids, and matching a CoreGraphics
//! display to its IORegistry framebuffer / AV service (pure logic, so it is
//! unit-tested on every platform).

/// Facts parsed from a base EDID block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edid {
    /// Three-letter PnP manufacturer id, e.g. "GSM" (LG), "DEL".
    pub manufacturer: String,
    /// The raw big-endian manufacturer word (what `CGDisplayVendorNumber` returns).
    pub vendor: u16,
    pub product: u16,
    pub serial_number: u32,
    pub week: u8,
    pub year: u16,
    /// Image size in cm.
    pub width_cm: u8,
    pub height_cm: u8,
    /// Monitor name descriptor (0xFC).
    pub name: Option<String>,
    /// Serial string descriptor (0xFF).
    pub serial_string: Option<String>,
}

const EDID_HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// Decode the 3-letter PnP id from the big-endian manufacturer word.
pub fn pnp_id(vendor: u16) -> String {
    let c = |shift: u16| {
        let v = ((vendor >> shift) & 0x1F) as u8;
        if (1..=26).contains(&v) {
            (b'A' + v - 1) as char
        } else {
            '?'
        }
    };
    [c(10), c(5), c(0)].iter().collect()
}

/// Parse an EDID base block (128+ bytes). The checksum is not enforced
/// (some displays get it wrong) but the header is.
pub fn parse_edid(b: &[u8]) -> Option<Edid> {
    if b.len() < 128 || b[..8] != EDID_HEADER {
        return None;
    }
    let vendor = u16::from_be_bytes([b[8], b[9]]);
    let mut e = Edid {
        manufacturer: pnp_id(vendor),
        vendor,
        product: u16::from_le_bytes([b[10], b[11]]),
        serial_number: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        week: b[16],
        year: 1990 + b[17] as u16,
        width_cm: b[21],
        height_cm: b[22],
        name: None,
        serial_string: None,
    };
    for d in (54..126).step_by(18) {
        let desc = &b[d..d + 18];
        if desc[0] != 0 || desc[1] != 0 || desc[2] != 0 {
            continue; // a detailed timing descriptor
        }
        let text = || {
            let raw = &desc[5..18];
            let end = raw.iter().position(|&c| c == 0x0A || c == 0).unwrap_or(raw.len());
            let s: String = raw[..end].iter().map(|&c| if c.is_ascii_graphic() || c == b' ' { c as char } else { '?' }).collect();
            let s = s.trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        match desc[3] {
            0xFC => e.name = text(),
            0xFF => e.serial_string = text(),
            _ => {}
        }
    }
    Some(e)
}

/// What a backend knows about a display when building its stable id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityFacts {
    pub name: String,
    /// Serial string or number as reported (EDID descriptor, IORegistry, …).
    pub serial: Option<String>,
    pub vendor: u32,
    pub model: u32,
    pub is_builtin: bool,
}

/// Stable display id, `"<name>#<serial>"`. Without a serial, the vendor and
/// model are used (`"<name>#v<vendor>m<model>"`); the built-in panel is
/// `"<name>#builtin"`. Ids survive reboots and reconnects, and only change
/// if two identical serial-less monitors swap ports.
pub fn stable_id(f: &IdentityFacts) -> String {
    let name = {
        let n: String = f.name.trim().chars().map(|c| if c == '#' { '_' } else { c }).collect();
        if n.is_empty() {
            "Display".to_string()
        } else {
            n
        }
    };
    let serial = f.serial.as_deref().map(str::trim).filter(|s| !s.is_empty() && *s != "0");
    match serial {
        Some(s) => format!("{name}#{s}"),
        None if f.is_builtin => format!("{name}#builtin"),
        None => format!("{name}#v{:04x}m{:04x}", f.vendor, f.model),
    }
}

/// Stable ids for a whole set of displays; duplicates get `"~2"`, `"~3"`
/// suffixes in enumeration order.
pub fn stable_ids(facts: &[IdentityFacts]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(facts.len());
    for f in facts {
        let base = stable_id(f);
        let mut id = base.clone();
        let mut n = 2;
        while out.contains(&id) {
            id = format!("{base}~{n}");
            n += 1;
        }
        out.push(id);
    }
    out
}

/// What CoreGraphics / CoreDisplay say about an online display.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CgFacts {
    pub vendor: u32,
    pub model: u32,
    pub serial: u32,
    /// `IODisplayLocation` from CoreDisplay's info dictionary (an IOService
    /// plane path of the framebuffer).
    pub location: Option<String>,
    pub week: Option<u32>,
    pub year: Option<u32>,
    /// Physical size in mm (`CGDisplayScreenSize`).
    pub width_mm: f64,
    pub height_mm: f64,
}

/// An IORegistry framebuffer (AppleCLCD2 / IOMobileFramebufferShim) that
/// has an external `DCPAVServiceProxy` below it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AvCandidate {
    /// IOService plane path of the framebuffer entry.
    pub path: String,
    /// `"EDID UUID"`, e.g. `"10AC2EA1-0000-0000-2E1D-0104B53C2278"`.
    pub edid_uuid: Option<String>,
    pub product_name: Option<String>,
    pub serial_number: Option<u32>,
}

fn hex2(v: u32) -> String {
    format!("{:02X}", v & 0xFF)
}

/// MonitorControl-style score of how well `cand` matches `cg`. A matching
/// `IODisplayLocation` alone is decisive (+10); the EDID UUID contributes
/// one point each for vendor, product, manufacture date and image size.
pub fn match_score(cg: &CgFacts, cand: &AvCandidate) -> i32 {
    let mut score = 0;
    if let Some(loc) = &cg.location {
        if !loc.is_empty() && !cand.path.is_empty() && (loc == &cand.path || loc.ends_with(&cand.path) || cand.path.ends_with(loc.as_str())) {
            score += 10;
        }
    }
    if let Some(uuid) = cand.edid_uuid.as_deref() {
        let u = uuid.to_ascii_uppercase();
        if u.len() == 36 && u.is_ascii() {
            if u[0..4] == format!("{:04X}", cg.vendor & 0xFFFF) {
                score += 1;
            }
            // Product id bytes as stored in the EDID (little-endian).
            if u[4..8] == format!("{}{}", hex2(cg.model), hex2(cg.model >> 8)) {
                score += 1;
            }
            if let (Some(w), Some(y)) = (cg.week, cg.year) {
                if y >= 1990 && u[19..23] == format!("{}{}", hex2(w), hex2(y - 1990)) {
                    score += 1;
                }
            }
            let (Ok(hw), Ok(hh)) = (u32::from_str_radix(&u[30..32], 16), u32::from_str_radix(&u[32..34], 16)) else { return score };
            let (cw, ch) = ((cg.width_mm / 10.0).round() as i64, (cg.height_mm / 10.0).round() as i64);
            if cg.width_mm > 0.0 && (hw as i64 - cw).abs() <= 1 && (hh as i64 - ch).abs() <= 1 {
                score += 1;
            }
        }
    }
    if let Some(sn) = cand.serial_number {
        if sn != 0 && sn == cg.serial {
            score += 1;
        }
    }
    score
}

/// Assign each CG display at most one candidate (and each candidate at most
/// one display), best scores first. A display only gets a candidate with a
/// positive score — except when there is exactly one of each, which is the
/// common single-external-monitor case.
pub fn assign(cgs: &[CgFacts], cands: &[AvCandidate]) -> Vec<Option<usize>> {
    let mut out = vec![None; cgs.len()];
    if cgs.len() == 1 && cands.len() == 1 {
        out[0] = Some(0);
        return out;
    }
    let mut pairs: Vec<(i32, usize, usize)> = Vec::new();
    for (i, cg) in cgs.iter().enumerate() {
        for (j, c) in cands.iter().enumerate() {
            let s = match_score(cg, c);
            if s > 0 {
                pairs.push((s, i, j));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut used = vec![false; cands.len()];
    for (_, i, j) in pairs {
        if out[i].is_none() && !used[j] {
            out[i] = Some(j);
            used[j] = true;
        }
    }
    out
}

/// Windows: registry key (under HKLM) holding the EDID of a monitor, from
/// its device interface path
/// (`\\?\DISPLAY#GSM5B09#5&2c1e1b4&0&UID4352#{e6f07b5f-…}` →
/// `SYSTEM\CurrentControlSet\Enum\DISPLAY\GSM5B09\5&2c1e1b4&0&UID4352\Device Parameters`).
pub fn edid_registry_key(device_path: &str) -> Option<String> {
    let p = device_path.strip_prefix(r"\\?\").unwrap_or(device_path);
    let mut parts = p.split('#');
    let class = parts.next()?;
    let model = parts.next()?;
    let instance = parts.next()?;
    if !class.eq_ignore_ascii_case("DISPLAY") || model.is_empty() || instance.is_empty() {
        return None;
    }
    Some(format!(r"SYSTEM\CurrentControlSet\Enum\DISPLAY\{model}\{instance}\Device Parameters"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_edid() -> Vec<u8> {
        let mut b = vec![0u8; 128];
        b[..8].copy_from_slice(&EDID_HEADER);
        // "GSM" = G(7) S(19) M(13) → 0b0_00111_10011_01101 = 0x1E6D
        b[8] = 0x1E;
        b[9] = 0x6D;
        b[10] = 0x09; // product 0x5B09 LE
        b[11] = 0x5B;
        b[12..16].copy_from_slice(&0x0001_E240u32.to_le_bytes());
        b[16] = 12;
        b[17] = 30; // 2020
        b[21] = 60;
        b[22] = 34;
        // Descriptor 2: name.
        let d = 72;
        b[d + 3] = 0xFC;
        b[d + 5..d + 18].copy_from_slice(b"LG ULTRAFINE\n");
        // Descriptor 3: serial.
        let d = 90;
        b[d + 3] = 0xFF;
        b[d + 5..d + 18].copy_from_slice(b"901NTAB12345\n");
        // Descriptor 1: a detailed timing (non-zero pixel clock) is skipped.
        b[54] = 0x01;
        b[54 + 3] = 0xFC;
        let sum: u32 = b[..127].iter().map(|&x| x as u32).sum();
        b[127] = ((256 - sum % 256) % 256) as u8;
        b
    }

    #[test]
    fn parses_edid() {
        let e = parse_edid(&sample_edid()).unwrap();
        assert_eq!(e.manufacturer, "GSM");
        assert_eq!(e.vendor, 0x1E6D);
        assert_eq!(e.product, 0x5B09);
        assert_eq!(e.serial_number, 123456);
        assert_eq!((e.week, e.year), (12, 2020));
        assert_eq!((e.width_cm, e.height_cm), (60, 34));
        assert_eq!(e.name.as_deref(), Some("LG ULTRAFINE"));
        assert_eq!(e.serial_string.as_deref(), Some("901NTAB12345"));
        assert!(parse_edid(&[0u8; 128]).is_none());
        assert!(parse_edid(&sample_edid()[..100]).is_none());
        assert_eq!(pnp_id(0x10AC), "DEL");
        assert_eq!(pnp_id(0x0610), "APP");
    }

    #[test]
    fn stable_ids_are_stable_and_unique() {
        let lg = IdentityFacts { name: "LG UltraFine".into(), serial: Some("901NTAB12345".into()), vendor: 0x1E6D, model: 0x5B09, is_builtin: false };
        assert_eq!(stable_id(&lg), "LG UltraFine#901NTAB12345");
        let noserial = IdentityFacts { name: " DELL U2720Q ".into(), serial: Some("0".into()), vendor: 0x10AC, model: 0xA12E, is_builtin: false };
        assert_eq!(stable_id(&noserial), "DELL U2720Q#v10acma12e");
        let builtin = IdentityFacts { name: "Built-in Retina Display".into(), serial: None, vendor: 0x610, model: 0xA050, is_builtin: true };
        assert_eq!(stable_id(&builtin), "Built-in Retina Display#builtin");
        let hash = IdentityFacts { name: "A#B".into(), serial: Some("1".into()), ..Default::default() };
        assert_eq!(stable_id(&hash), "A_B#1");
        let empty = IdentityFacts::default();
        assert_eq!(stable_id(&empty), "Display#v0000m0000");
        let ids = stable_ids(&[noserial.clone(), lg.clone(), noserial.clone(), noserial]);
        assert_eq!(ids, vec!["DELL U2720Q#v10acma12e", "LG UltraFine#901NTAB12345", "DELL U2720Q#v10acma12e~2", "DELL U2720Q#v10acma12e~3"]);
    }

    #[test]
    fn edid_registry_keys() {
        assert_eq!(
            edid_registry_key(r"\\?\DISPLAY#GSM5B09#5&2c1e1b4&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}").as_deref(),
            Some(r"SYSTEM\CurrentControlSet\Enum\DISPLAY\GSM5B09\5&2c1e1b4&0&UID4352\Device Parameters")
        );
        assert_eq!(edid_registry_key(r"\\?\USB#VID_1#2#{x}"), None);
        assert_eq!(edid_registry_key(""), None);
    }

    fn dell_cg() -> CgFacts {
        CgFacts {
            vendor: 0x10AC,
            model: 0xA12E,
            serial: 0x4C4C4C4C,
            location: Some("IOService:/AppleARMPE/arm-io@10F00000/AppleT600xIO/dispext0@A0000000/IOMobileFramebufferShim".into()),
            week: Some(0x2E),
            year: Some(1990 + 0x1D),
            width_mm: 597.0,
            height_mm: 336.0,
        }
    }

    #[test]
    fn scores_edid_uuid_and_location() {
        let cg = dell_cg();
        let by_uuid = AvCandidate { path: "IOService:/x/dispext1@0/IOMobileFramebufferShim".into(), edid_uuid: Some("10AC2EA1-0000-0000-2E1D-0104B53C2278".into()), ..Default::default() };
        assert_eq!(match_score(&cg, &by_uuid), 4);
        let by_loc = AvCandidate { path: cg.location.clone().unwrap(), edid_uuid: None, ..Default::default() };
        assert_eq!(match_score(&cg, &by_loc), 10);
        let other = AvCandidate { path: "IOService:/y".into(), edid_uuid: Some("1E6D095B-0000-0000-0C1E-0104B53C2278".into()), ..Default::default() };
        assert_eq!(match_score(&cg, &other), 1, "only the image size matches");
        let lower = AvCandidate { edid_uuid: Some("10ac2ea1-0000-0000-2e1d-0104b53c2278".into()), ..Default::default() };
        assert_eq!(match_score(&cg, &lower), 4);
        let sn = AvCandidate { serial_number: Some(0x4C4C4C4C), ..Default::default() };
        assert_eq!(match_score(&cg, &sn), 1);
    }

    #[test]
    fn assigns_uniquely() {
        let dell = dell_cg();
        let lg = CgFacts { vendor: 0x1E6D, model: 0x5B09, ..Default::default() };
        let c_lg = AvCandidate { path: "a".into(), edid_uuid: Some("1E6D095B-0000-0000-0000-000000000000".into()), ..Default::default() };
        let c_dell = AvCandidate { path: "b".into(), edid_uuid: Some("10AC2EA1-0000-0000-2E1D-0104B53C2278".into()), ..Default::default() };
        assert_eq!(assign(&[dell.clone(), lg.clone()], &[c_lg.clone(), c_dell.clone()]), vec![Some(1), Some(0)]);
        // Unknown third display gets nothing.
        let other = CgFacts { vendor: 1, model: 2, ..Default::default() };
        assert_eq!(assign(&[other.clone(), dell.clone()], std::slice::from_ref(&c_dell)), vec![None, Some(0)]);
        // One of each: paired even without evidence.
        assert_eq!(assign(&[other], &[c_lg]), vec![Some(0)]);
        assert_eq!(assign(&[], &[c_dell]), Vec::<Option<usize>>::new());
    }
}
