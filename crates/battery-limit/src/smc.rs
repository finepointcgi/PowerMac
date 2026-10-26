//! System Management Controller access.
//!
//! [`SmcBackend`] is what the scheme detection and the privileged helper
//! talk to. [`AppleSmc`] (macOS) is the real AppleSMC user client; reads work
//! without privileges, writes need root (the kernel returns
//! `kIOReturnNotPrivileged` otherwise). [`FakeSmc`] is a JSON-file-backed
//! stand-in used by tests and by the helper's `--fake-smc` test mode.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Size and type of an SMC key, as returned by `getKeyInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInfo {
    pub size: u32,
    /// Four-character type code, e.g. `ui8 `, `ui32`, `hex_`, `flt `.
    pub data_type: String,
}

/// Something that can read and write SMC keys.
pub trait SmcBackend: Send {
    /// `Ok(None)` when the key doesn't exist.
    fn key_info(&mut self, key: &str) -> Result<Option<KeyInfo>>;
    fn read_key(&mut self, key: &str) -> Result<Vec<u8>>;
    fn write_key(&mut self, key: &str, bytes: &[u8]) -> Result<()>;
}

/// `"CHTE"` → 0x43485445.
pub fn fourcc(key: &str) -> Result<u32> {
    let b = key.as_bytes();
    if b.len() != 4 || !b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
        bail!("invalid SMC key {key:?}");
    }
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn fourcc_to_string(v: u32) -> String {
    v.to_be_bytes()
        .iter()
        .map(|&c| {
            if c.is_ascii_graphic() || c == b' ' {
                c as char
            } else {
                '?'
            }
        })
        .collect()
}

/// Best-effort decoding of an SMC value for display (temperatures etc.).
pub fn decode_number(info: &KeyInfo, bytes: &[u8]) -> Option<f64> {
    let t = info.data_type.as_str();
    match (t, bytes.len()) {
        // Apple Silicon: little-endian IEEE float.
        ("flt ", 4) => Some(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64),
        // Intel: signed fixed point 7.8, big-endian.
        ("sp78", 2) => Some(i16::from_be_bytes([bytes[0], bytes[1]]) as f64 / 256.0),
        ("ui8 " | "hex_" | "flag", 1) => Some(bytes[0] as f64),
        ("ui16", 2) => Some(u16::from_be_bytes([bytes[0], bytes[1]]) as f64),
        ("ui32", 4) => Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64),
        _ => None,
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Fake backend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FakeSmcState {
    pub keys: BTreeMap<String, FakeKey>,
    /// Every write, in order: (key, hex bytes).
    pub writes: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakeKey {
    pub data_type: String,
    pub bytes: Vec<u8>,
}

/// SMC stand-in persisted to a JSON file after every write, so a test can
/// inspect what a helper process did even after it exited.
pub struct FakeSmc {
    path: Option<PathBuf>,
    pub state: FakeSmcState,
}

impl FakeSmc {
    /// In-memory fake with the keys of a firmware generation:
    /// `"tahoe"` (CHTE + CHIE), `"legacy"` (CH0B/CH0C + CH0I), `"none"`.
    pub fn preset(name: &str) -> Result<FakeSmc> {
        let mut keys = BTreeMap::new();
        let mut add = |k: &str, t: &str, b: Vec<u8>| {
            keys.insert(
                k.to_string(),
                FakeKey {
                    data_type: t.into(),
                    bytes: b,
                },
            );
        };
        match name {
            "tahoe" => {
                add("CHTE", "ui32", vec![0, 0, 0, 0]);
                add("CHIE", "hex_", vec![0]);
                add("ACLC", "ui8 ", vec![0]);
            }
            "legacy" => {
                add("CH0B", "hex_", vec![0]);
                add("CH0C", "hex_", vec![0]);
                add("CH0I", "hex_", vec![0]);
            }
            "none" => {}
            other => bail!("unknown fake SMC preset {other:?}"),
        }
        add("TB0T", "flt ", 30.5f32.to_le_bytes().to_vec());
        Ok(FakeSmc {
            path: None,
            state: FakeSmcState {
                keys,
                writes: vec![],
            },
        })
    }

    /// Create a preset and persist it at `path`.
    pub fn create(path: &Path, preset: &str) -> Result<FakeSmc> {
        let mut f = Self::preset(preset)?;
        f.path = Some(path.to_path_buf());
        f.save()?;
        Ok(f)
    }

    pub fn open(path: &Path) -> Result<FakeSmc> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(FakeSmc {
            path: Some(path.to_path_buf()),
            state: serde_json::from_str(&text)?,
        })
    }

    fn save(&self) -> Result<()> {
        if let Some(p) = &self.path {
            let tmp = p.with_extension("tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&self.state)?)?;
            std::fs::rename(tmp, p)?;
        }
        Ok(())
    }
}

impl SmcBackend for FakeSmc {
    fn key_info(&mut self, key: &str) -> Result<Option<KeyInfo>> {
        Ok(self.state.keys.get(key).map(|k| KeyInfo {
            size: k.bytes.len() as u32,
            data_type: k.data_type.clone(),
        }))
    }

    fn read_key(&mut self, key: &str) -> Result<Vec<u8>> {
        self.state
            .keys
            .get(key)
            .map(|k| k.bytes.clone())
            .ok_or_else(|| anyhow!("key {key} not found"))
    }

    fn write_key(&mut self, key: &str, bytes: &[u8]) -> Result<()> {
        let k = self
            .state
            .keys
            .get_mut(key)
            .ok_or_else(|| anyhow!("key {key} not found"))?;
        if k.bytes.len() != bytes.len() {
            bail!("size mismatch for {key}");
        }
        k.bytes = bytes.to_vec();
        self.state.writes.push((key.to_string(), hex(bytes)));
        self.save()
    }
}

// ---------------------------------------------------------------------------
// Real AppleSMC (macOS)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub use real::AppleSmc;

#[cfg(target_os = "macos")]
mod real {
    use super::*;
    use crate::platform::macos_ffi::*;
    use std::ffi::c_void;

    const KERNEL_INDEX_SMC: u32 = 2; // kSMCHandleYPCEvent
    const SMC_CMD_READ_KEY: u8 = 5;
    const SMC_CMD_WRITE_KEY: u8 = 6;
    const SMC_CMD_GET_KEY_INFO: u8 = 9;
    const SMC_CMD_GET_KEY_FROM_INDEX: u8 = 8;
    const SMC_RESULT_KEY_NOT_FOUND: u8 = 0x84;
    const IO_RETURN_NOT_PRIVILEGED: i32 = 0xe00002c1_u32 as i32;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Vers {
        major: u8,
        minor: u8,
        build: u8,
        reserved: u8,
        release: u16,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct PLimit {
        version: u16,
        length: u16,
        cpu: u32,
        gpu: u32,
        mem: u32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct KInfo {
        data_size: u32,
        data_type: u32,
        data_attributes: u8,
    }

    /// `SMCParamStruct` from the AppleSMC user client (80 bytes).
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Param {
        key: u32,
        vers: Vers,
        p_limit: PLimit,
        key_info: KInfo,
        result: u8,
        status: u8,
        data8: u8,
        data32: u32,
        bytes: [u8; 32],
    }

    const _: () = assert!(std::mem::size_of::<Param>() == 80);

    /// An open connection to the AppleSMC user client.
    pub struct AppleSmc {
        conn: IoConnect,
    }

    // The connection is a mach port name; usable from any thread.
    unsafe impl Send for AppleSmc {}

    impl AppleSmc {
        pub fn open() -> Result<AppleSmc> {
            let svc =
                IoObj::matching("AppleSMC").ok_or_else(|| anyhow!("AppleSMC service not found"))?;
            let mut conn: IoConnect = 0;
            // SAFETY: valid service; mach_task_self_ is this task's port.
            let kr = unsafe { IOServiceOpen(svc.0, mach_task_self_, 0, &mut conn) };
            if kr != KERN_SUCCESS {
                bail!("IOServiceOpen(AppleSMC) failed: {kr:#x}");
            }
            Ok(AppleSmc { conn })
        }

        fn call(&self, input: &Param) -> Result<Param> {
            let mut out = Param::default();
            let mut size = std::mem::size_of::<Param>();
            // SAFETY: both buffers are 80-byte SMCParamStructs.
            let kr = unsafe {
                IOConnectCallStructMethod(
                    self.conn,
                    KERNEL_INDEX_SMC,
                    input as *const Param as *const c_void,
                    std::mem::size_of::<Param>(),
                    &mut out as *mut Param as *mut c_void,
                    &mut size,
                )
            };
            if kr == IO_RETURN_NOT_PRIVILEGED {
                bail!("SMC call not permitted (needs root)");
            }
            if kr != KERN_SUCCESS {
                bail!("IOConnectCallStructMethod failed: {kr:#x}");
            }
            Ok(out)
        }

        /// Number of keys (`#KEY`).
        pub fn key_count(&mut self) -> Result<u32> {
            let b = self.read_key("#KEY")?;
            if b.len() != 4 {
                bail!("#KEY has {} bytes", b.len());
            }
            Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        }

        /// The key at `index` (0..key_count), for enumeration. Read-only.
        pub fn key_at_index(&mut self, index: u32) -> Result<String> {
            let p = Param {
                data8: SMC_CMD_GET_KEY_FROM_INDEX,
                data32: index,
                ..Default::default()
            };
            let out = self.call(&p)?;
            if out.result != 0 {
                bail!("SMC getKeyFromIndex({index}) result {:#x}", out.result);
            }
            Ok(fourcc_to_string(out.key))
        }

        fn info_raw(&self, key: &str) -> Result<Option<KInfo>> {
            let p = Param {
                key: fourcc(key)?,
                data8: SMC_CMD_GET_KEY_INFO,
                ..Default::default()
            };
            let out = self.call(&p)?;
            match out.result {
                0 => Ok(Some(out.key_info)),
                SMC_RESULT_KEY_NOT_FOUND => Ok(None),
                r => bail!("SMC getKeyInfo({key}) result {r:#x}"),
            }
        }
    }

    impl Drop for AppleSmc {
        fn drop(&mut self) {
            // SAFETY: we opened this connection.
            unsafe {
                IOServiceClose(self.conn);
            }
        }
    }

    impl SmcBackend for AppleSmc {
        fn key_info(&mut self, key: &str) -> Result<Option<KeyInfo>> {
            Ok(self.info_raw(key)?.map(|i| KeyInfo {
                size: i.data_size,
                data_type: fourcc_to_string(i.data_type),
            }))
        }

        fn read_key(&mut self, key: &str) -> Result<Vec<u8>> {
            let info = self
                .info_raw(key)?
                .ok_or_else(|| anyhow!("SMC key {key} not found"))?;
            let size = info.data_size.min(32);
            let p = Param {
                key: fourcc(key)?,
                key_info: KInfo {
                    data_size: size,
                    ..info
                },
                data8: SMC_CMD_READ_KEY,
                ..Default::default()
            };
            let out = self.call(&p)?;
            if out.result != 0 {
                bail!("SMC read({key}) result {:#x}", out.result);
            }
            Ok(out.bytes[..size as usize].to_vec())
        }

        fn write_key(&mut self, key: &str, bytes: &[u8]) -> Result<()> {
            let info = self
                .info_raw(key)?
                .ok_or_else(|| anyhow!("SMC key {key} not found"))?;
            if info.data_size as usize != bytes.len() {
                bail!(
                    "SMC write({key}): expected {} bytes, got {}",
                    info.data_size,
                    bytes.len()
                );
            }
            let mut p = Param {
                key: fourcc(key)?,
                key_info: KInfo {
                    data_size: info.data_size,
                    ..info
                },
                data8: SMC_CMD_WRITE_KEY,
                ..Default::default()
            };
            p.bytes[..bytes.len()].copy_from_slice(bytes);
            let out = self.call(&p)?;
            if out.result != 0 {
                bail!("SMC write({key}) result {:#x}", out.result);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_roundtrip() {
        assert_eq!(fourcc("CHTE").unwrap(), 0x4348_5445);
        assert_eq!(fourcc_to_string(0x4348_5445), "CHTE");
        assert_eq!(fourcc_to_string(fourcc("ui8 ").unwrap()), "ui8 ");
        assert!(fourcc("CHT").is_err());
        assert!(fourcc("CHTEX").is_err());
    }

    #[test]
    fn decode_values() {
        let flt = KeyInfo {
            size: 4,
            data_type: "flt ".into(),
        };
        assert_eq!(decode_number(&flt, &31.5f32.to_le_bytes()), Some(31.5));
        let sp = KeyInfo {
            size: 2,
            data_type: "sp78".into(),
        };
        assert_eq!(decode_number(&sp, &[0x1e, 0x80]), Some(30.5));
    }

    #[test]
    fn fake_rejects_unknown_and_wrong_size() {
        let mut f = FakeSmc::preset("tahoe").unwrap();
        assert!(f.write_key("CH0B", &[2]).is_err());
        assert!(f.write_key("CHTE", &[1]).is_err());
        f.write_key("CHTE", &[1, 0, 0, 0]).unwrap();
        assert_eq!(f.read_key("CHTE").unwrap(), vec![1, 0, 0, 0]);
        assert_eq!(
            f.state.writes,
            vec![("CHTE".to_string(), "01000000".to_string())]
        );
    }
}
