//! Bluetooth device batteries: parsing `system_profiler SPBluetoothDataType
//! -json` (AirPods left / right / case, Magic devices) and merging it with
//! the IOKit HID battery readings the platform layer collects.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BtKind {
    AirPods,
    AirPodsPro,
    AirPodsMax,
    Headphones,
    Mouse,
    Keyboard,
    Trackpad,
    Controller,
    Speaker,
    Other,
}

impl BtKind {
    pub fn symbol(self) -> &'static str {
        match self {
            BtKind::AirPods => "airpods",
            BtKind::AirPodsPro => "airpodspro",
            BtKind::AirPodsMax => "airpodsmax",
            BtKind::Headphones => "headphones",
            BtKind::Mouse => "magicmouse.fill",
            BtKind::Keyboard => "keyboard.fill",
            BtKind::Trackpad => "rectangle.and.hand.point.up.left.fill",
            BtKind::Controller => "gamecontroller.fill",
            BtKind::Speaker => "hifispeaker.fill",
            BtKind::Other => "dot.radiowaves.left.and.right",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Main,
    Left,
    Right,
    Case,
}

impl Part {
    pub fn label(self) -> &'static str {
        match self {
            Part::Main => "",
            Part::Left => "L",
            Part::Right => "R",
            Part::Case => "Case",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BtDevice {
    pub name: String,
    pub kind: BtKind,
    /// Normalized ("aa:bb:cc:dd:ee:ff"), may be empty.
    pub address: String,
    pub levels: Vec<(Part, u8)>,
}

impl BtDevice {
    /// Lowest level (what the card's ring shows for a multi-part device).
    pub fn min_level(&self) -> Option<u8> {
        self.levels.iter().filter(|(p, _)| *p != Part::Case).map(|(_, l)| *l).min().or_else(|| self.levels.iter().map(|(_, l)| *l).min())
    }
}

pub fn normalize_address(a: &str) -> String {
    let hex: String = a.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_ascii_lowercase();
    if hex.len() != 12 {
        return String::new();
    }
    (0..6).map(|i| &hex[i * 2..i * 2 + 2]).collect::<Vec<_>>().join(":")
}

/// Guess the kind from the name, minor type and Apple product id.
pub fn classify(name: &str, minor_type: &str, product_id: Option<u32>) -> BtKind {
    let n = name.to_ascii_lowercase();
    let t = minor_type.to_ascii_lowercase();
    match product_id {
        Some(0x200e | 0x2014 | 0x2024 | 0x2027) => return BtKind::AirPodsPro,
        Some(0x200a | 0x201f) => return BtKind::AirPodsMax,
        Some(0x2002 | 0x200f | 0x2013 | 0x2019 | 0x201b | 0x2026) => return BtKind::AirPods,
        _ => {}
    }
    if n.contains("airpods pro") {
        BtKind::AirPodsPro
    } else if n.contains("airpods max") {
        BtKind::AirPodsMax
    } else if n.contains("airpods") {
        BtKind::AirPods
    } else if n.contains("trackpad") || t.contains("trackpad") {
        BtKind::Trackpad
    } else if n.contains("mouse") || t.contains("mouse") {
        BtKind::Mouse
    } else if n.contains("keyboard") || t.contains("keyboard") {
        BtKind::Keyboard
    } else if n.contains("controller") || t.contains("gamepad") || t.contains("joystick") || n.contains("dualsense") || n.contains("xbox") {
        BtKind::Controller
    } else if t.contains("headphones") || t.contains("headset") || n.contains("beats") || n.contains("buds") {
        BtKind::Headphones
    } else if t.contains("speaker") || n.contains("speaker") {
        BtKind::Speaker
    } else {
        BtKind::Other
    }
}

fn pct(v: Option<&Value>) -> Option<u8> {
    let s = match v? {
        Value::String(s) => s.trim().trim_end_matches('%').trim().parse::<f64>().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (0.0..=100.0).contains(&s).then_some(s.round() as u8)
}

fn hex_u32(v: Option<&Value>) -> Option<u32> {
    let s = v?.as_str()?.trim();
    u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}

/// Connected devices with a battery from `system_profiler
/// SPBluetoothDataType -json`.
pub fn parse_system_profiler(json: &str) -> Vec<BtDevice> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return vec![] };
    let mut out = vec![];
    for ctl in v.get("SPBluetoothDataType").and_then(|a| a.as_array()).into_iter().flatten() {
        for key in ["device_connected", "devices_list"] {
            for entry in ctl.get(key).and_then(|a| a.as_array()).into_iter().flatten() {
                let Some(obj) = entry.as_object() else { continue };
                for (name, d) in obj {
                    // Older macOS lists every device under devices_list with a
                    // connection flag.
                    if key == "devices_list" && d.get("device_connected").and_then(|c| c.as_str()).is_some_and(|c| !c.contains("yes")) {
                        continue;
                    }
                    let mut levels = vec![];
                    for (k, part) in [("device_batteryLevelMain", Part::Main), ("device_batteryLevel", Part::Main), ("device_batteryLevelLeft", Part::Left), ("device_batteryLevelRight", Part::Right), ("device_batteryLevelCase", Part::Case)] {
                        if let Some(l) = pct(d.get(k)) {
                            if !levels.iter().any(|(p, _)| *p == part) {
                                levels.push((part, l));
                            }
                        }
                    }
                    if levels.is_empty() {
                        continue;
                    }
                    let minor = d.get("device_minorType").and_then(|m| m.as_str()).unwrap_or("");
                    out.push(BtDevice {
                        name: name.clone(),
                        kind: classify(name, minor, hex_u32(d.get("device_productID"))),
                        address: normalize_address(d.get("device_address").and_then(|a| a.as_str()).unwrap_or("")),
                        levels,
                    });
                }
            }
        }
    }
    out
}

/// A HID device's battery as IOKit reports it
/// (`AppleDeviceManagementHIDEventService` / `BatteryPercent`).
#[derive(Clone, Debug, PartialEq)]
pub struct HidBattery {
    pub name: String,
    pub address: String,
    pub percent: u8,
}

/// Merge both sources: system_profiler first (it knows the AirPods
/// parts), HID readings for devices it missed or to fill a main level.
/// Filtered by `only` (names, case-insensitive substring; empty = all),
/// sorted by name.
pub fn merge(profiler: Vec<BtDevice>, hid: &[HidBattery], only: &[String]) -> Vec<BtDevice> {
    let mut out = profiler;
    for h in hid {
        let addr = normalize_address(&h.address);
        let found = out.iter_mut().find(|d| (!addr.is_empty() && d.address == addr) || d.name.eq_ignore_ascii_case(&h.name));
        match found {
            Some(d) => {
                if !d.levels.iter().any(|(p, _)| *p == Part::Main) && d.levels.is_empty() {
                    d.levels.push((Part::Main, h.percent));
                }
            }
            None => out.push(BtDevice { name: h.name.clone(), kind: classify(&h.name, "", None), address: addr, levels: vec![(Part::Main, h.percent)] }),
        }
    }
    out.retain(|d| only.is_empty() || only.iter().any(|o| !o.trim().is_empty() && d.name.to_ascii_lowercase().contains(&o.trim().to_ascii_lowercase())));
    out.sort_by_key(|a| a.name.to_ascii_lowercase());
    out
}

/// Ring color bucket: low (≤ 20 %), medium, ok.
pub fn level_is_low(l: u8) -> bool {
    l <= 20
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shape of macOS 14/15 output (trimmed).
    const SONOMA: &str = r#"{"SPBluetoothDataType":[{"controller_properties":{"controller_address":"F0:2F:4B:00:00:00","controller_state":"attrib_on"},
      "device_connected":[
        {"Mitch's AirPods Pro":{"device_address":"AC:90:85:12:34:56","device_batteryLevelCase":"45%","device_batteryLevelLeft":"100%","device_batteryLevelRight":"98 %","device_minorType":"Headphones","device_productID":"0x2014","device_vendorID":"0x004C"}},
        {"Magic Mouse":{"device_address":"04:4B:ED:AA:BB:CC","device_batteryLevelMain":"71%","device_minorType":"Mouse","device_productID":"0x0269"}},
        {"MX Keys":{"device_address":"D1:22:33:44:55:66","device_minorType":"Keyboard"}}
      ],
      "device_not_connected":[{"Old Speaker":{"device_address":"00:11:22:33:44:55","device_batteryLevelMain":"10%"}}]}]}"#;

    #[test]
    fn parses_system_profiler() {
        let d = parse_system_profiler(SONOMA);
        assert_eq!(d.len(), 2, "no battery → skipped; not connected → skipped");
        assert_eq!(d[0].kind, BtKind::AirPodsPro);
        assert_eq!(d[0].address, "ac:90:85:12:34:56");
        assert_eq!(d[0].levels, vec![(Part::Left, 100), (Part::Right, 98), (Part::Case, 45)]);
        assert_eq!(d[0].min_level(), Some(98));
        assert_eq!(d[1].kind, BtKind::Mouse);
        assert_eq!(d[1].levels, vec![(Part::Main, 71)]);
        assert!(parse_system_profiler("not json").is_empty());
        assert!(parse_system_profiler("{}").is_empty());
        // Older layout.
        let old = r#"{"SPBluetoothDataType":[{"devices_list":[{"Keyboard":{"device_connected":"attrib_yes","device_batteryLevel":"55%","device_address":"x"}},{"Gone":{"device_connected":"attrib_no","device_batteryLevel":"5%"}}]}]}"#;
        let d = parse_system_profiler(old);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].kind, d[0].levels[0]), (BtKind::Keyboard, (Part::Main, 55)));
    }

    #[test]
    fn merges_and_filters() {
        let prof = parse_system_profiler(SONOMA);
        let hid = vec![
            HidBattery { name: "Magic Mouse".into(), address: "04-4b-ed-aa-bb-cc".into(), percent: 70 },
            HidBattery { name: "Magic Trackpad".into(), address: "".into(), percent: 15 },
        ];
        let m = merge(prof.clone(), &hid, &[]);
        assert_eq!(m.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["Magic Mouse", "Magic Trackpad", "Mitch's AirPods Pro"]);
        assert_eq!(m[0].levels, vec![(Part::Main, 71)], "system_profiler wins");
        assert_eq!(m[1].kind, BtKind::Trackpad);
        assert!(level_is_low(m[1].levels[0].1));
        let f = merge(prof, &hid, &["airpods".into()]);
        assert_eq!(f.len(), 1);
        assert_eq!(normalize_address("AC-90-85-12-34-5"), "");
    }

    #[test]
    fn classification() {
        assert_eq!(classify("AirPods Max", "", None), BtKind::AirPodsMax);
        assert_eq!(classify("Thing", "", Some(0x2002)), BtKind::AirPods);
        assert_eq!(classify("Xbox Wireless Controller", "Gamepad", None), BtKind::Controller);
        assert_eq!(classify("Galaxy Buds2", "", None), BtKind::Headphones);
        assert_eq!(classify("Pen", "", None), BtKind::Other);
        assert_eq!(BtKind::Trackpad.symbol(), "rectangle.and.hand.point.up.left.fill");
    }
}
