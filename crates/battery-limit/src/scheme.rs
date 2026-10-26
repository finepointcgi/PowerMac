//! Which SMC keys control charging on this Mac, and what to write to them.
//!
//! * Apple Silicon on recent firmware (macOS 15+): `CHTE` (ui32, 4 bytes):
//!   `00000000` = charging allowed, `01000000` = charging inhibited.
//! * Older firmware (Apple Silicon on macOS ≤ 14, and T2/Intel Macs that have
//!   them): `CH0B` and `CH0C` (1 byte each): `00` = allowed, `02` = inhibited.
//! * Adapter / "discharge while plugged in": `CHIE` (1 byte): `08` = adapter
//!   off (battery powers the Mac), `00` = normal; older firmware `CH0I`
//!   (or `CH0J`): `01` = adapter off, `00` = normal.
//! * `ACLC` (MagSafe LED) is detected but never written.
//!
//! Intel Macs without these keys used `BCLM` (charge level max, set once in
//! firmware). That is detected for reporting only.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::smc::SmcBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ChargeKeys {
    /// `CHTE`
    Chte,
    /// `CH0B` + `CH0C`
    Ch0bCh0c,
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

impl Scheme {
    /// Probe the backend with `getKeyInfo` (unprivileged for these keys on
    /// the Macs seen so far) and pick the keys. A key only counts if its size
    /// matches what we would write; a key whose info can't be read (some
    /// keys need root even for that) counts as absent.
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
            "CHTE", "CH0B", "CH0C", "CHIE", "CH0I", "CH0J", "ACLC", "BCLM",
        ] {
            sizes.insert(k, exists(k).map(|i| i.size));
        }
        let has = |key: &str, size: u32| sizes[key] == Some(size);
        let charge = if has("CHTE", 4) {
            ChargeKeys::Chte
        } else if has("CH0B", 1) && has("CH0C", 1) {
            ChargeKeys::Ch0bCh0c
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

    /// Short human/wire name, e.g. `CHTE+CHIE`, `CH0B/CH0C+CH0I`, `none`.
    pub fn name(&self) -> String {
        let c = match self.charge {
            ChargeKeys::Chte => "CHTE",
            ChargeKeys::Ch0bCh0c => "CH0B/CH0C",
            ChargeKeys::None => "none",
        };
        match self.adapter {
            AdapterKeys::Chie => format!("{c}+CHIE"),
            AdapterKeys::Ch0i => format!("{c}+CH0I"),
            AdapterKeys::Ch0j => format!("{c}+CH0J"),
            AdapterKeys::None => c.to_string(),
        }
    }

    /// Writes that allow (`true`) or inhibit (`false`) charging.
    pub fn charging_writes(&self, allow: bool) -> Vec<Write> {
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
    /// charging allowed.
    pub fn restore_writes(&self) -> Vec<Write> {
        let mut w = self.discharge_writes(false);
        w.extend(self.charging_writes(true));
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smc::FakeSmc;

    #[test]
    fn detects_tahoe() {
        let mut f = FakeSmc::preset("tahoe").unwrap();
        let s = Scheme::detect(&mut f).unwrap();
        assert_eq!(s.charge, ChargeKeys::Chte);
        assert_eq!(s.adapter, AdapterKeys::Chie);
        assert!(s.magsafe_led);
        assert_eq!(s.name(), "CHTE+CHIE");
        assert_eq!(s.charging_writes(false), vec![("CHTE", vec![1, 0, 0, 0])]);
        assert_eq!(s.charging_writes(true), vec![("CHTE", vec![0, 0, 0, 0])]);
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
            s.charging_writes(false),
            vec![("CH0B", vec![2]), ("CH0C", vec![2])]
        );
        assert_eq!(s.discharge_writes(true), vec![("CH0I", vec![1])]);
    }

    #[test]
    fn detects_none_and_wrong_sizes() {
        let mut f = FakeSmc::preset("none").unwrap();
        let s = Scheme::detect(&mut f).unwrap();
        assert!(!s.can_inhibit() && !s.can_discharge());
        assert!(s.restore_writes().is_empty());
        assert_eq!(s.name(), "none");

        // CHTE with an unexpected size is not trusted.
        let mut f = FakeSmc::preset("none").unwrap();
        f.state.keys.insert(
            "CHTE".into(),
            crate::smc::FakeKey {
                data_type: "ui8 ".into(),
                bytes: vec![0],
            },
        );
        assert_eq!(Scheme::detect(&mut f).unwrap().charge, ChargeKeys::None);
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
    fn prefers_chte_when_both_exist() {
        let mut f = FakeSmc::preset("legacy").unwrap();
        f.state.keys.insert(
            "CHTE".into(),
            crate::smc::FakeKey {
                data_type: "ui32".into(),
                bytes: vec![0; 4],
            },
        );
        assert_eq!(Scheme::detect(&mut f).unwrap().charge, ChargeKeys::Chte);
    }
}
