//! Which SMC keys control charging on this Mac, what to write to them, and
//! which limiting [`Strategy`] that allows.
//!
//! | Keys | Firmware | Meaning / values |
//! |------|----------|------------------|
//! | `CHTE` (ui32, 4 B) | Apple Silicon, macOS 15 – 26.6 | `00000000` charging allowed, `01000000` inhibited |
//! | `CH0B` + `CH0C` (1 B each) | Apple Silicon ≤ macOS 14, T2/Intel | `00` allowed, `02` inhibited |
//! | `bfF0` (ui8) + `bfD0`/`bfE0` (ui32, little-endian %) | "macOS 27-era" firmware (also shipped in 15.8 / 26.7 updates) | firmware-enforced charge range: `bfF0=00`, `bfD0=upper`, `bfE0=lower`, `bfF0=02` (in that order) activates; `bfF0=00` deactivates. On macOS 27 beta 4+ / firmware 20457.x these keys are gated behind a private entitlement and fail with `kIOReturnNotPrivileged` even as root — then they count as absent. |
//! | `CHIE` (1 B) | Apple Silicon, macOS 15+ (still ungated on macOS 27) | adapter off (`08`) / normal (`00`) |
//! | `CH0I` / `CH0J` (1 B) | older firmware | adapter off (`01`) / normal (`00`) |
//!
//! `ACLC` (MagSafe LED) and `BCLM` (Intel firmware charge maximum) are
//! detected for reporting only and never written.
//!
//! Sources: actuallymentor/battery `battery.sh` v1.3.4 (CHTE/CH0B/CH0C,
//! CHIE/CH0J/CH0I values), charlie0129/batt `pkg/smc/charging.go`
//! (bfF0/bfD0/bfE0 encoding and write order) and batt issue #152 (macOS 27
//! gating).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::smc::SmcBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ChargeKeys {
    /// `CHTE`
    Chte,
    /// `CH0B` + `CH0C`
    Ch0bCh0c,
    /// `bfF0` + `bfD0` + `bfE0` (firmware-enforced range)
    FirmwareRange,
    #[default]
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AdapterKeys {
    /// `CHIE`
    Chie,
    /// `CH0I`
    Ch0i,
    /// `CH0J`
    Ch0j,
    #[default]
    None,
}

/// Detected key set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Scheme {
    pub charge: ChargeKeys,
    pub adapter: AdapterKeys,
    /// `ACLC` exists (MagSafe LED); informational.
    pub magsafe_led: bool,
    /// `BCLM` exists (Intel firmware charge limit); informational.
    pub bclm: bool,
}

/// One SMC write.
pub type Write = (&'static str, Vec<u8>);

/// A charge range for the firmware-range scheme: charging stops at `upper`
/// and resumes below `lower`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub lower: u8,
    pub upper: u8,
}

impl Default for Range {
    fn default() -> Self {
        Range {
            lower: 75,
            upper: 80,
        }
    }
}

impl Range {
    /// `10 ≤ lower < upper ≤ 100`.
    pub fn new(lower: u8, upper: u8) -> Result<Range> {
        if lower < 10 || upper > 100 || lower >= upper {
            bail!("invalid range {lower}-{upper}");
        }
        Ok(Range { lower, upper })
    }

    /// Range for a limit and sailing band (at least 1 % wide).
    pub fn for_limit(limit: u8, sailing: u8) -> Range {
        let upper = limit.clamp(20, 100);
        let lower = upper.saturating_sub(sailing.max(1)).max(10);
        Range { lower, upper }
    }

    /// `"75-80"`.
    pub fn to_wire(self) -> String {
        format!("{}-{}", self.lower, self.upper)
    }

    pub fn from_wire(s: &str) -> Result<Range> {
        let Some((l, u)) = s.split_once('-') else {
            bail!("bad range {s:?}");
        };
        Range::new(l.parse()?, u.parse()?)
    }
}

/// How the charge limit is enforced on this Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Strategy {
    /// Charging inhibited directly (`CHTE` / `CH0B`+`CH0C`); the adapter
    /// keeps powering the Mac.
    Inhibit,
    /// The firmware enforces a charge range (`bfD0`/`bfE0`/`bfF0`).
    FirmwareRange,
    /// No inhibit key: the adapter is switched off at the limit (battery
    /// powers the Mac and slowly discharges) and back on below the sailing
    /// threshold.
    LimitViaDischarge,
    /// Nothing usable.
    None,
}

impl Strategy {
    /// Short label for the status readout.
    pub fn label(self) -> &'static str {
        match self {
            Strategy::Inhibit => "charge inhibit",
            Strategy::FirmwareRange => "firmware charge range",
            Strategy::LimitViaDischarge => "limit via discharge",
            Strategy::None => "unavailable",
        }
    }

    /// What the user should know about this strategy.
    pub fn note(self) -> &'static str {
        match self {
            Strategy::Inhibit => {
                "Charging is paused at the limit; the Mac keeps running from the adapter."
            }
            Strategy::FirmwareRange => {
                "The battery firmware stops charging at the limit and resumes below the \
                 sailing threshold; it holds during sleep when \"keep limit during sleep\" is on. \
                 Heat protection can only pause charging at the limit with this scheme."
            }
            Strategy::LimitViaDischarge => {
                "This Mac's firmware has no usable charge-inhibit key, so at the limit the \
                 power adapter is switched off (CHIE) and the Mac runs from its battery until \
                 it drops below the sailing threshold, then the adapter is switched back on. \
                 Trade-offs: the battery does shallow cycles between the two thresholds (more \
                 cycle count than a true inhibit, but less time at high charge); the adapter \
                 is always switched back on before sleep, on quit and if PowerMac stops \
                 responding, so a sleeping Mac charges normally; macOS shows the Mac as on \
                 battery while the adapter is off. For a firmware-enforced 80–100 % limit use \
                 System Settings → Battery → Charge Limit."
            }
            Strategy::None => {
                "This Mac's firmware exposes no known charge-inhibit or adapter key that \
                 PowerMac can use."
            }
        }
    }
}

impl Scheme {
    /// Probe the backend with `getKeyInfo` (unprivileged for these keys on
    /// the Macs seen so far) and pick the keys. A key only counts if its size
    /// matches what we would write; a key whose info can't be read (gated
    /// keys need root, or an entitlement, even for that) counts as absent.
    pub fn detect(smc: &mut dyn SmcBackend) -> Result<Scheme> {
        let mut exists = |key: &str| match smc.key_info(key) {
            Ok(i) => i,
            Err(e) => {
                log::debug!("SMC {key}: {e:#}");
                None
            }
        };
        let mut sizes = std::collections::HashMap::new();
        for k in [
            "CHTE", "CH0B", "CH0C", "bfF0", "bfD0", "bfE0", "CHIE", "CH0I", "CH0J", "ACLC", "BCLM",
        ] {
            sizes.insert(k, exists(k).map(|i| i.size));
        }
        let has = |key: &str, size: u32| sizes[key] == Some(size);
        // Direct inhibit first (verified, and lets heat protection pause
        // charging anywhere); then the firmware range.
        let charge = if has("CHTE", 4) {
            ChargeKeys::Chte
        } else if has("CH0B", 1) && has("CH0C", 1) {
            ChargeKeys::Ch0bCh0c
        } else if has("bfF0", 1) && has("bfD0", 4) && has("bfE0", 4) {
            ChargeKeys::FirmwareRange
        } else {
            ChargeKeys::None
        };
        let adapter = if has("CHIE", 1) {
            AdapterKeys::Chie
        } else if has("CH0I", 1) {
            AdapterKeys::Ch0i
        } else if has("CH0J", 1) {
            AdapterKeys::Ch0j
        } else {
            AdapterKeys::None
        };
        let magsafe_led = sizes["ACLC"].is_some();
        let bclm = sizes["BCLM"].is_some();
        Ok(Scheme {
            charge,
            adapter,
            magsafe_led,
            bclm,
        })
    }

    pub fn can_inhibit(&self) -> bool {
        self.charge != ChargeKeys::None
    }

    pub fn can_discharge(&self) -> bool {
        self.adapter != AdapterKeys::None
    }

    /// How a limit would be enforced. `allow_discharge_fallback`: use the
    /// adapter-off strategy when there is no inhibit key.
    pub fn strategy(&self, allow_discharge_fallback: bool) -> Strategy {
        match self.charge {
            ChargeKeys::Chte | ChargeKeys::Ch0bCh0c => Strategy::Inhibit,
            ChargeKeys::FirmwareRange => Strategy::FirmwareRange,
            ChargeKeys::None if allow_discharge_fallback && self.can_discharge() => {
                Strategy::LimitViaDischarge
            }
            ChargeKeys::None => Strategy::None,
        }
    }

    /// Short human/wire name, e.g. `CHTE+CHIE`, `CH0B/CH0C+CH0I`,
    /// `bfD0/bfE0/bfF0+CHIE`, `none+CHIE`, `none`.
    pub fn name(&self) -> String {
        let c = match self.charge {
            ChargeKeys::Chte => "CHTE",
            ChargeKeys::Ch0bCh0c => "CH0B/CH0C",
            ChargeKeys::FirmwareRange => "bfD0/bfE0/bfF0",
            ChargeKeys::None => "none",
        };
        match self.adapter {
            AdapterKeys::Chie => format!("{c}+CHIE"),
            AdapterKeys::Ch0i => format!("{c}+CH0I"),
            AdapterKeys::Ch0j => format!("{c}+CH0J"),
            AdapterKeys::None => c.to_string(),
        }
    }

    /// Inverse of [`Scheme::name`] (informational flags are not encoded).
    /// Unknown parts parse as `None`.
    pub fn from_name(name: &str) -> Scheme {
        let (c, a) = name.split_once('+').unwrap_or((name, ""));
        let charge = match c {
            "CHTE" => ChargeKeys::Chte,
            "CH0B/CH0C" => ChargeKeys::Ch0bCh0c,
            "bfD0/bfE0/bfF0" => ChargeKeys::FirmwareRange,
            _ => ChargeKeys::None,
        };
        let adapter = match a {
            "CHIE" => AdapterKeys::Chie,
            "CH0I" => AdapterKeys::Ch0i,
            "CH0J" => AdapterKeys::Ch0j,
            _ => AdapterKeys::None,
        };
        Scheme {
            charge,
            adapter,
            ..Default::default()
        }
    }

    /// Writes that allow (`true`) or inhibit (`false`) charging. `range` is
    /// only used by the firmware-range scheme.
    pub fn charging_writes(&self, allow: bool, range: Range) -> Vec<Write> {
        match self.charge {
            ChargeKeys::Chte => vec![(
                "CHTE",
                if allow {
                    vec![0, 0, 0, 0]
                } else {
                    vec![1, 0, 0, 0]
                },
            )],
            ChargeKeys::Ch0bCh0c => {
                let v = if allow { 0x00 } else { 0x02 };
                vec![("CH0B", vec![v]), ("CH0C", vec![v])]
            }
            ChargeKeys::FirmwareRange => {
                if allow {
                    vec![("bfF0", vec![0x00])]
                } else {
                    // Order required by the firmware: deactivate, upper,
                    // lower, activate. Percentages are little-endian ui32.
                    vec![
                        ("bfF0", vec![0x00]),
                        ("bfD0", (range.upper as u32).to_le_bytes().to_vec()),
                        ("bfE0", (range.lower as u32).to_le_bytes().to_vec()),
                        ("bfF0", vec![0x02]),
                    ]
                }
            }
            ChargeKeys::None => vec![],
        }
    }

    /// Writes that turn the adapter off (`true`, battery discharges while
    /// plugged in) or back to normal (`false`).
    pub fn discharge_writes(&self, discharge: bool) -> Vec<Write> {
        match self.adapter {
            AdapterKeys::Chie => vec![("CHIE", vec![if discharge { 0x08 } else { 0x00 }])],
            AdapterKeys::Ch0i => vec![("CH0I", vec![if discharge { 0x01 } else { 0x00 }])],
            AdapterKeys::Ch0j => vec![("CH0J", vec![if discharge { 0x01 } else { 0x00 }])],
            AdapterKeys::None => vec![],
        }
    }

    /// Writes that put everything back to the firmware default: adapter on,
    /// charging allowed (firmware range deactivated).
    pub fn restore_writes(&self) -> Vec<Write> {
        let mut w = self.discharge_writes(false);
        w.extend(self.charging_writes(true, Range::default()));
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smc::FakeSmc;

    const R: Range = Range {
        lower: 75,
        upper: 80,
    };

    #[test]
    fn detects_tahoe() {
        let mut f = FakeSmc::preset("tahoe").unwrap();
        let s = Scheme::detect(&mut f).unwrap();
        assert_eq!(s.charge, ChargeKeys::Chte);
        assert_eq!(s.adapter, AdapterKeys::Chie);
        assert!(s.magsafe_led);
        assert_eq!(s.name(), "CHTE+CHIE");
        assert_eq!(s.strategy(true), Strategy::Inhibit);
        assert_eq!(
            s.charging_writes(false, R),
            vec![("CHTE", vec![1, 0, 0, 0])]
        );
        assert_eq!(s.charging_writes(true, R), vec![("CHTE", vec![0, 0, 0, 0])]);
        assert_eq!(s.discharge_writes(true), vec![("CHIE", vec![8])]);
        assert_eq!(
            s.restore_writes(),
            vec![("CHIE", vec![0]), ("CHTE", vec![0, 0, 0, 0])]
        );
    }

    #[test]
    fn detects_legacy() {
        let mut f = FakeSmc::preset("legacy").unwrap();
        let s = Scheme::detect(&mut f).unwrap();
        assert_eq!(s.charge, ChargeKeys::Ch0bCh0c);
        assert_eq!(s.adapter, AdapterKeys::Ch0i);
        assert_eq!(s.name(), "CH0B/CH0C+CH0I");
        assert_eq!(
            s.charging_writes(false, R),
            vec![("CH0B", vec![2]), ("CH0C", vec![2])]
        );
        assert_eq!(s.discharge_writes(true), vec![("CH0I", vec![1])]);
    }

    #[test]
    fn detects_firmware_range() {
        let mut f = FakeSmc::preset("range").unwrap();
        let s = Scheme::detect(&mut f).unwrap();
        assert_eq!(s.charge, ChargeKeys::FirmwareRange);
        assert_eq!(s.name(), "bfD0/bfE0/bfF0+CHIE");
        assert_eq!(s.strategy(true), Strategy::FirmwareRange);
        assert_eq!(
            s.charging_writes(false, Range::new(70, 80).unwrap()),
            vec![
                ("bfF0", vec![0]),
                ("bfD0", vec![80, 0, 0, 0]),
                ("bfE0", vec![70, 0, 0, 0]),
                ("bfF0", vec![2]),
            ]
        );
        assert_eq!(s.charging_writes(true, R), vec![("bfF0", vec![0])]);
        assert_eq!(
            s.restore_writes(),
            vec![("CHIE", vec![0]), ("bfF0", vec![0])]
        );
    }

    /// The M4 Pro / macOS 27.2 this was developed on: CHTE/CH0B/CH0C absent,
    /// bfD0/bfE0/bfF0, CH0J and CHLS gated (key info fails), CHIE readable.
    #[test]
    fn gated_mac_falls_back_to_discharge() {
        let mut f = FakeSmc::preset("gated").unwrap();
        let s = Scheme::detect(&mut f).unwrap();
        assert_eq!(s.charge, ChargeKeys::None);
        assert_eq!(s.adapter, AdapterKeys::Chie);
        assert_eq!(s.name(), "none+CHIE");
        assert_eq!(s.strategy(true), Strategy::LimitViaDischarge);
        assert_eq!(s.strategy(false), Strategy::None);
        assert!(s.charging_writes(false, R).is_empty());
        assert_eq!(s.restore_writes(), vec![("CHIE", vec![0])]);
    }

    #[test]
    fn detects_none_and_wrong_sizes() {
        let mut f = FakeSmc::preset("none").unwrap();
        let s = Scheme::detect(&mut f).unwrap();
        assert!(!s.can_inhibit() && !s.can_discharge());
        assert!(s.restore_writes().is_empty());
        assert_eq!(s.name(), "none");
        assert_eq!(s.strategy(true), Strategy::None);

        // CHTE with an unexpected size (macOS 27 leaves zero-size
        // placeholders) is not trusted.
        for bytes in [vec![0], vec![]] {
            let mut f = FakeSmc::preset("none").unwrap();
            f.state.keys.insert(
                "CHTE".into(),
                crate::smc::FakeKey {
                    data_type: "ui32".into(),
                    bytes,
                },
            );
            assert_eq!(Scheme::detect(&mut f).unwrap().charge, ChargeKeys::None);
        }
    }

    #[test]
    fn info_errors_count_as_absent() {
        struct Denied;
        impl SmcBackend for Denied {
            fn key_info(&mut self, _: &str) -> Result<Option<crate::smc::KeyInfo>> {
                anyhow::bail!("SMC call not permitted (needs root)")
            }
            fn read_key(&mut self, _: &str) -> Result<Vec<u8>> {
                unreachable!()
            }
            fn write_key(&mut self, _: &str, _: &[u8]) -> Result<()> {
                unreachable!()
            }
        }
        assert_eq!(Scheme::detect(&mut Denied).unwrap(), Scheme::default());
    }

    #[test]
    fn prefers_direct_inhibit() {
        let mut f = FakeSmc::preset("legacy").unwrap();
        f.state.keys.insert(
            "CHTE".into(),
            crate::smc::FakeKey {
                data_type: "ui32".into(),
                bytes: vec![0; 4],
            },
        );
        assert_eq!(Scheme::detect(&mut f).unwrap().charge, ChargeKeys::Chte);
        let mut f = FakeSmc::preset("range").unwrap();
        f.state.keys.insert(
            "CHTE".into(),
            crate::smc::FakeKey {
                data_type: "ui32".into(),
                bytes: vec![0; 4],
            },
        );
        assert_eq!(Scheme::detect(&mut f).unwrap().charge, ChargeKeys::Chte);
    }

    #[test]
    fn name_roundtrip_and_ranges() {
        for p in ["tahoe", "legacy", "range", "gated", "none"] {
            let s = Scheme::detect(&mut FakeSmc::preset(p).unwrap()).unwrap();
            let back = Scheme::from_name(&s.name());
            assert_eq!((back.charge, back.adapter), (s.charge, s.adapter), "{p}");
        }
        assert_eq!(Range::for_limit(80, 5), R);
        assert_eq!(Range::for_limit(80, 0), Range::new(79, 80).unwrap());
        assert_eq!(Range::from_wire("75-80").unwrap(), R);
        assert_eq!(R.to_wire(), "75-80");
        for bad in ["80-75", "5-50", "50-101", "x", "50-"] {
            assert!(Range::from_wire(bad).is_err(), "{bad}");
        }
    }
}
