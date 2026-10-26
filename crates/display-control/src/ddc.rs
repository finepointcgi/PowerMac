//! DDC/CI (VESA MCCS over I2C) packet encoding and decoding.
//!
//! Only the pure byte-level parts live here; the transports are in the
//! platform backends (IOAVService on Apple Silicon, Dxva2 on Windows, which
//! does its own framing).
//!
//! Wire format, host → display (I2C write to 7-bit address 0x37 = 0x6E):
//!
//! ```text
//! 0x6E  0x51  0x80|len  <len payload bytes>  checksum
//! ```
//!
//! `0x51` is the host's source address, sent as the I2C "sub-address" by
//! `IOAVServiceWriteI2C(svc, 0x37, 0x51, buf, n)`, so `buf` starts at the
//! length byte. The checksum is the XOR of every byte including `0x6E` and
//! `0x51`.
//!
//! Display → host (I2C read from 0x6F):
//!
//! ```text
//! 0x6E  0x80|len  <len payload bytes>  checksum
//! ```
//!
//! where the checksum is the XOR of all bytes seeded with the "virtual host
//! address" `0x50`.

use serde::{Deserialize, Serialize};

/// 7-bit I2C address of the DDC/CI channel.
pub const DDC_I2C_ADDR: u8 = 0x37;
/// Host source address (also the I2C sub-address used for writes).
pub const HOST_ADDR: u8 = 0x51;
/// Display destination address (`0x37 << 1`).
pub const DISPLAY_ADDR: u8 = 0x6E;
/// Seed of the reply checksum.
pub const REPLY_CHECKSUM_SEED: u8 = 0x50;

const OP_GET_VCP: u8 = 0x01;
const OP_GET_VCP_REPLY: u8 = 0x02;
const OP_SET_VCP: u8 = 0x03;

/// Size of the buffer to read a "Get VCP Feature" reply into.
pub const GET_REPLY_LEN: usize = 11;

/// A controllable monitor feature (MCCS VCP code).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Feature {
    Brightness,
    Contrast,
    Volume,
    Mute,
    InputSource,
}

impl Feature {
    pub const ALL: [Feature; 5] = [Feature::Brightness, Feature::Contrast, Feature::Volume, Feature::Mute, Feature::InputSource];

    /// MCCS VCP code.
    pub const fn vcp(self) -> u8 {
        match self {
            Feature::Brightness => 0x10,
            Feature::Contrast => 0x12,
            Feature::Volume => 0x62,
            Feature::Mute => 0x8D,
            Feature::InputSource => 0x60,
        }
    }

    pub fn from_vcp(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.vcp() == code)
    }

    /// Features shown as a 0–100 % slider.
    pub const fn is_continuous(self) -> bool {
        matches!(self, Feature::Brightness | Feature::Contrast | Feature::Volume)
    }
}

/// MCCS 0x8D values.
pub const MUTE_ON: u16 = 1;
pub const MUTE_OFF: u16 = 2;

/// XOR checksum seeded with `seed`.
pub fn checksum(seed: u8, bytes: &[u8]) -> u8 {
    bytes.iter().fold(seed, |acc, b| acc ^ b)
}

/// "Set VCP Feature" payload (starts at the length byte).
pub fn encode_set_vcp(vcp: u8, value: u16) -> [u8; 6] {
    let mut p = [0x84, OP_SET_VCP, vcp, (value >> 8) as u8, (value & 0xFF) as u8, 0];
    p[5] = checksum(DISPLAY_ADDR ^ HOST_ADDR, &p[..5]);
    p
}

/// "Get VCP Feature" request payload (starts at the length byte).
pub fn encode_get_vcp(vcp: u8) -> [u8; 4] {
    let mut p = [0x82, OP_GET_VCP, vcp, 0];
    p[3] = checksum(DISPLAY_ADDR ^ HOST_ADDR, &p[..3]);
    p
}

/// A decoded "Get VCP Feature" reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VcpReply {
    pub vcp: u8,
    /// 0 = set parameter, 1 = momentary.
    pub kind: u8,
    pub max: u16,
    pub current: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdcError {
    /// Buffer too short or wrong length byte.
    Malformed,
    /// The display answered with a null message (busy; try again).
    Null,
    BadChecksum,
    /// Some other opcode than a Get VCP reply.
    NotAReply,
    /// The reply is for a different VCP code (a stale answer).
    WrongCode(u8),
    /// The display says it doesn't support this VCP code.
    Unsupported,
    /// All-zero / all-0xFF buffer: nothing answered.
    NoResponse,
}

impl std::fmt::Display for DdcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DdcError::Malformed => write!(f, "malformed DDC reply"),
            DdcError::Null => write!(f, "display sent a DDC null message (busy)"),
            DdcError::BadChecksum => write!(f, "DDC reply checksum mismatch"),
            DdcError::NotAReply => write!(f, "unexpected DDC opcode"),
            DdcError::WrongCode(c) => write!(f, "DDC reply for another VCP code (0x{c:02X})"),
            DdcError::Unsupported => write!(f, "VCP code not supported by the display"),
            DdcError::NoResponse => write!(f, "no DDC response"),
        }
    }
}

impl std::error::Error for DdcError {}

/// Whether retrying the same request could help.
pub fn is_transient(e: DdcError) -> bool {
    !matches!(e, DdcError::Unsupported)
}

/// Decode the bytes read after a "Get VCP Feature" request for `vcp`.
///
/// `buf` normally starts with the display's source address `0x6E`; buffers
/// with that byte stripped (starting at the length byte) are accepted too.
pub fn decode_get_vcp_reply(buf: &[u8], vcp: u8) -> Result<VcpReply, DdcError> {
    if buf.is_empty() || buf.iter().all(|&b| b == 0) || buf.iter().all(|&b| b == 0xFF) {
        return Err(DdcError::NoResponse);
    }
    // The checksum always covers the source address 0x6E, read or not.
    let frame = if buf[0] == DISPLAY_ADDR { &buf[1..] } else { buf };
    let seed = REPLY_CHECKSUM_SEED ^ DISPLAY_ADDR;
    let Some(&len_byte) = frame.first() else { return Err(DdcError::Malformed) };
    if len_byte & 0x80 == 0 {
        return Err(DdcError::Malformed);
    }
    let len = (len_byte & 0x7F) as usize;
    if frame.len() < len + 2 {
        return Err(DdcError::Malformed);
    }
    let body = &frame[..=len];
    if checksum(seed, body) != frame[len + 1] {
        return Err(DdcError::BadChecksum);
    }
    if len == 0 {
        return Err(DdcError::Null);
    }
    if len != 8 || frame[1] != OP_GET_VCP_REPLY {
        return Err(DdcError::NotAReply);
    }
    let result = frame[2];
    let code = frame[3];
    if code != vcp {
        return Err(DdcError::WrongCode(code));
    }
    if result != 0 {
        return Err(DdcError::Unsupported);
    }
    Ok(VcpReply {
        vcp: code,
        kind: frame[4],
        max: u16::from_be_bytes([frame[5], frame[6]]),
        current: u16::from_be_bytes([frame[7], frame[8]]),
    })
}

/// Build a full reply buffer (as read from the display); used by tests and
/// the fake backend.
pub fn encode_get_vcp_reply(vcp: u8, max: u16, current: u16, unsupported: bool) -> [u8; GET_REPLY_LEN] {
    let mut r = [
        DISPLAY_ADDR,
        0x88,
        OP_GET_VCP_REPLY,
        unsupported as u8,
        vcp,
        0,
        (max >> 8) as u8,
        (max & 0xFF) as u8,
        (current >> 8) as u8,
        (current & 0xFF) as u8,
        0,
    ];
    r[10] = checksum(REPLY_CHECKSUM_SEED, &r[..10]);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_request_matches_spec_example() {
        // DDC/CI spec: 6E 51 82 01 10 AC — "get brightness".
        assert_eq!(encode_get_vcp(0x10), [0x82, 0x01, 0x10, 0xAC]);
        let full = [DISPLAY_ADDR, HOST_ADDR, 0x82, 0x01, 0x10];
        assert_eq!(checksum(0, &full), 0xAC);
    }

    #[test]
    fn set_request_checksum() {
        let p = encode_set_vcp(0x10, 50);
        assert_eq!(&p[..5], &[0x84, 0x03, 0x10, 0x00, 0x32]);
        assert_eq!(p[5], checksum(0, &[0x6E, 0x51, 0x84, 0x03, 0x10, 0x00, 0x32]));
        assert_eq!(p[5], 0x9A);
        let p = encode_set_vcp(0x62, 0x1234);
        assert_eq!(&p[3..5], &[0x12, 0x34]);
        assert_eq!(checksum(DISPLAY_ADDR ^ HOST_ADDR, &p), 0, "checksum makes the whole frame XOR to zero");
    }

    #[test]
    fn decode_reply_roundtrip() {
        let r = encode_get_vcp_reply(0x10, 100, 37, false);
        assert_eq!(r[10], checksum(0x50, &[0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 100, 0x00, 37]));
        assert_eq!(r[10], 0x50 ^ 0x6E ^ 0x88 ^ 0x02 ^ 0x10 ^ 100 ^ 37);
        let v = decode_get_vcp_reply(&r, 0x10).unwrap();
        assert_eq!(v, VcpReply { vcp: 0x10, kind: 0, max: 100, current: 37 });
        // Without the leading source address.
        assert_eq!(decode_get_vcp_reply(&r[1..], 0x10).unwrap(), v);
        // Trailing junk (12-byte reads) is ignored.
        let mut long = r.to_vec();
        long.push(0xAB);
        assert_eq!(decode_get_vcp_reply(&long, 0x10).unwrap(), v);
        // 16-bit values.
        let r = encode_get_vcp_reply(0x62, 0x0400, 0x0123, false);
        let v = decode_get_vcp_reply(&r, 0x62).unwrap();
        assert_eq!((v.max, v.current), (0x400, 0x123));
    }

    #[test]
    fn decode_errors() {
        let mut r = encode_get_vcp_reply(0x10, 100, 37, false);
        assert_eq!(decode_get_vcp_reply(&r, 0x12), Err(DdcError::WrongCode(0x10)));
        r[9] ^= 1;
        assert_eq!(decode_get_vcp_reply(&r, 0x10), Err(DdcError::BadChecksum));
        let r = encode_get_vcp_reply(0x8D, 2, 1, true);
        assert_eq!(decode_get_vcp_reply(&r, 0x8D), Err(DdcError::Unsupported));
        assert_eq!(decode_get_vcp_reply(&[0; 11], 0x10), Err(DdcError::NoResponse));
        assert_eq!(decode_get_vcp_reply(&[0xFF; 11], 0x10), Err(DdcError::NoResponse));
        assert_eq!(decode_get_vcp_reply(&[0x6E, 0x88, 0x02], 0x10), Err(DdcError::Malformed));
        // Null message: 6E 80 BE (0x50 ^ 0x6E ^ 0x80).
        assert_eq!(decode_get_vcp_reply(&[0x6E, 0x80, 0x50 ^ 0x6E ^ 0x80, 0, 0], 0x10), Err(DdcError::Null));
        assert!(!is_transient(DdcError::Unsupported));
        assert!(is_transient(DdcError::BadChecksum));
    }

    #[test]
    fn feature_codes() {
        assert_eq!(Feature::Brightness.vcp(), 0x10);
        assert_eq!(Feature::Contrast.vcp(), 0x12);
        assert_eq!(Feature::Volume.vcp(), 0x62);
        assert_eq!(Feature::Mute.vcp(), 0x8D);
        assert_eq!(Feature::InputSource.vcp(), 0x60);
        for f in Feature::ALL {
            assert_eq!(Feature::from_vcp(f.vcp()), Some(f));
        }
        assert_eq!(Feature::from_vcp(0x99), None);
    }
}
