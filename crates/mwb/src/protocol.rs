//! Byte-exact model of the MWB `DATA` packet (Core/DATA.cs) and related constants.
//!
//! `DATA` is a C# `[StructLayout(LayoutKind.Explicit)]` union. All integers are
//! little-endian. Offsets:
//!
//! | off | size | field                                                          |
//! |-----|------|----------------------------------------------------------------|
//! | 0   | 4    | `Type` (byte 0 = type, byte 1 = checksum, bytes 2..3 = magic)  |
//! | 4   | 4    | `Id` (package sequence number, used for de-duplication)        |
//! | 8   | 4    | `Src` (sender machine id)                                      |
//! | 12  | 4    | `Des` (destination machine id, 255 = ALL)                      |
//! | 16  | 8    | `DateTime` (union with Md / Machine1..4 / PostAction)          |
//! | 16  | 16   | `Md`: X, Y, WheelDelta, dwFlags (i32 each)                     |
//! | 24  | 8    | `Kd`: wVk, dwFlags (i32 each)                                  |
//! | 16  | 16   | `Machine1..Machine4` (u32 each)                                |
//! | 32  | 32   | machine name, ASCII, right-padded with spaces (big packets)    |
//!
//! A "small" package is 32 bytes on the wire, a "big" package 64 bytes
//! (see [`is_big_package`]).

pub const PACKAGE_SIZE: usize = 32;
pub const PACKAGE_SIZE_EX: usize = 64;

/// `ID.NONE` / `ID.ALL` (Core/ID.cs).
pub const ID_NONE: u32 = 0;
pub const ID_ALL: u32 = 255;

/// `Event.MOVE_MOUSE_RELATIVE`: relative deltas are offset by this amount.
pub const MOVE_MOUSE_RELATIVE: i32 = 100_000;
/// `Event.XY_BY_PIXEL`.
pub const XY_BY_PIXEL: i32 = 300_000;

/// Package types (Core/PackageType.cs).
#[allow(non_upper_case_globals, dead_code)]
pub mod ptype {
    pub const Invalid: u32 = 0xFF;
    pub const Error: u32 = 0xFE;
    pub const Hi: u32 = 2;
    pub const Hello: u32 = 3;
    pub const ByeBye: u32 = 4;
    pub const Heartbeat: u32 = 20;
    pub const Awake: u32 = 21;
    pub const HideMouse: u32 = 50;
    pub const Heartbeat_ex: u32 = 51;
    pub const Heartbeat_ex_l2: u32 = 52;
    pub const Heartbeat_ex_l3: u32 = 53;
    pub const Clipboard: u32 = 69;
    pub const ClipboardDragDrop: u32 = 70;
    pub const ClipboardDragDropEnd: u32 = 71;
    pub const ExplorerDragDrop: u32 = 72;
    pub const ClipboardCapture: u32 = 73;
    pub const CaptureScreenCommand: u32 = 74;
    pub const ClipboardDragDropOperation: u32 = 75;
    pub const ClipboardDataEnd: u32 = 76;
    pub const MachineSwitched: u32 = 77;
    pub const ClipboardAsk: u32 = 78;
    pub const ClipboardPush: u32 = 79;
    pub const NextMachine: u32 = 121;
    pub const Keyboard: u32 = 122;
    pub const Mouse: u32 = 123;
    pub const ClipboardText: u32 = 124;
    pub const ClipboardImage: u32 = 125;
    pub const Handshake: u32 = 126;
    pub const HandshakeAck: u32 = 127;
    pub const Matrix: u32 = 128;
    pub const MatrixSwapFlag: u32 = 2;
    pub const MatrixTwoRowFlag: u32 = 4;
}

/// Windows mouse message ids carried in `Md.dwFlags` (Core/WM.cs).
pub mod wm {
    pub const WM_MOUSEMOVE: i32 = 0x200;
    pub const WM_LBUTTONDOWN: i32 = 0x201;
    pub const WM_LBUTTONUP: i32 = 0x202;
    pub const WM_RBUTTONDOWN: i32 = 0x204;
    pub const WM_RBUTTONUP: i32 = 0x205;
    pub const WM_MBUTTONDOWN: i32 = 0x207;
    pub const WM_MBUTTONUP: i32 = 0x208;
    pub const WM_MOUSEWHEEL: i32 = 0x20A;
    pub const WM_XBUTTONDOWN: i32 = 0x20B;
    pub const WM_XBUTTONUP: i32 = 0x20C;
    pub const WM_MOUSEHWHEEL: i32 = 0x20E;
    /// `XBUTTON1` / `XBUTTON2` values stored in `Md.WheelDelta` for X-button messages.
    pub const XBUTTON1: i32 = 1;
    pub const XBUTTON2: i32 = 2;
}

/// `KBDLLHOOKSTRUCT.flags` bits carried in `Kd.dwFlags`.
pub mod llkhf {
    pub const EXTENDED: i32 = 0x01;
    pub const INJECTED: i32 = 0x10;
    pub const ALTDOWN: i32 = 0x20;
    pub const UP: i32 = 0x80;
}

/// `DATA.IsBigPackage`. Returns `None` for type 0 (C# throws `InvalidOperationException`).
///
/// Note the C# quirk: anything with bit 0x80 set (including `Invalid` = 0xFF and
/// `Error` = 0xFE) is treated as big because of the `Matrix` flag test.
#[allow(non_upper_case_globals)]
pub fn is_big_package(t: u32) -> Option<bool> {
    use ptype::*;
    if t == 0 {
        return None;
    }
    Some(match t {
        Hello | Awake | Heartbeat | Heartbeat_ex | Handshake | HandshakeAck | ClipboardPush
        | Clipboard | ClipboardAsk | ClipboardImage | ClipboardText | ClipboardDataEnd => true,
        _ => (t & Matrix) == Matrix,
    })
}

/// A 64-byte `DATA` buffer with typed accessors at the C# offsets.
#[derive(Clone, PartialEq, Eq)]
pub struct Data {
    pub buf: [u8; PACKAGE_SIZE_EX],
}

impl Default for Data {
    fn default() -> Self {
        Data { buf: [0; PACKAGE_SIZE_EX] }
    }
}

impl std::fmt::Debug for Data {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Data")
            .field("type", &self.ptype())
            .field("id", &self.id())
            .field("src", &self.src())
            .field("des", &self.des())
            .field("md", &(self.md_x(), self.md_y(), self.md_wheel(), self.md_flags()))
            .field("name", &self.machine_name())
            .finish()
    }
}

macro_rules! field_i32 {
    ($get:ident, $set:ident, $off:expr) => {
        pub fn $get(&self) -> i32 {
            self.get_i32($off)
        }
        pub fn $set(&mut self, v: i32) {
            self.set_i32($off, v)
        }
    };
}
macro_rules! field_u32 {
    ($get:ident, $set:ident, $off:expr) => {
        pub fn $get(&self) -> u32 {
            self.get_u32($off)
        }
        pub fn $set(&mut self, v: u32) {
            self.set_u32($off, v)
        }
    };
}

impl Data {
    pub fn new(t: u32) -> Self {
        let mut d = Data::default();
        d.set_type(t);
        d
    }

    /// Like C# `new DATA(byte[])`: copies up to 64 bytes, zero-filling the rest.
    pub fn from_bytes(b: &[u8]) -> Self {
        let mut d = Data::default();
        let n = b.len().min(PACKAGE_SIZE_EX);
        d.buf[..n].copy_from_slice(&b[..n]);
        d
    }

    pub fn get_i32(&self, off: usize) -> i32 {
        i32::from_le_bytes(self.buf[off..off + 4].try_into().unwrap())
    }
    pub fn set_i32(&mut self, off: usize, v: i32) {
        self.buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    pub fn get_u32(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.buf[off..off + 4].try_into().unwrap())
    }
    pub fn set_u32(&mut self, off: usize, v: u32) {
        self.buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// Full 32-bit `Type` field. After reception bytes 1..3 are zeroed, so this is the type byte.
    pub fn ptype(&self) -> u32 {
        self.get_u32(0)
    }
    pub fn set_type(&mut self, t: u32) {
        self.set_u32(0, t)
    }
    field_i32!(id, set_id, 4);
    field_u32!(src, set_src, 8);
    field_u32!(des, set_des, 12);

    pub fn date_time(&self) -> i64 {
        i64::from_le_bytes(self.buf[16..24].try_into().unwrap())
    }
    pub fn set_date_time(&mut self, v: i64) {
        self.buf[16..24].copy_from_slice(&v.to_le_bytes());
    }

    // MOUSEDATA at 16
    field_i32!(md_x, set_md_x, 16);
    field_i32!(md_y, set_md_y, 20);
    field_i32!(md_wheel, set_md_wheel, 24);
    field_i32!(md_flags, set_md_flags, 28);

    // KEYBDDATA at 24
    field_i32!(kd_vk, set_kd_vk, 24);
    field_i32!(kd_flags, set_kd_flags, 28);

    // Machine1..4 at 16..32
    pub fn machine(&self, i: usize) -> u32 {
        self.get_u32(16 + 4 * i)
    }
    pub fn set_machine(&mut self, i: usize, v: u32) {
        self.set_u32(16 + 4 * i, v)
    }

    /// `DATA.MachineName` getter: ASCII bytes 32..64, `Trim()`med.
    pub fn machine_name(&self) -> String {
        // ASCIIEncoding.GetString maps bytes > 0x7F to '?'.
        let s: String = self.buf[32..64]
            .iter()
            .map(|&b| if b < 0x80 { b as char } else { '?' })
            .collect();
        dotnet_trim(&s).to_string()
    }

    /// `DATA.MachineName` setter: `PadRight(32, ' ')`, ASCII-encoded, first 32 bytes kept.
    pub fn set_machine_name(&mut self, name: &str) {
        let mut bytes: Vec<u8> = name
            .chars()
            .map(|c| if (c as u32) < 0x80 { c as u8 } else { b'?' })
            .collect();
        while bytes.len() < 32 {
            bytes.push(b' ');
        }
        self.buf[32..64].copy_from_slice(&bytes[..32]);
    }

    /// Bytes as serialized by the C# `DATA.Bytes` getter (32 or 64 bytes).
    /// Returns `None` if the type is 0 (C# would throw).
    pub fn wire_bytes(&self) -> Option<&[u8]> {
        let big = is_big_package(self.ptype())?;
        Some(if big { &self.buf[..] } else { &self.buf[..PACKAGE_SIZE] })
    }

    pub fn is_big(&self) -> bool {
        is_big_package(self.ptype()).unwrap_or(false)
    }
}

/// .NET `string.Trim()` trims Unicode whitespace; for our ASCII content it is
/// space, \t, \n, \v, \f, \r (and NUL is *not* trimmed).
pub fn dotnet_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

/// `TcpSendData`: stamp the magic number into bytes 2..3 and the checksum into byte 1.
/// The checksum is the byte-sum of bytes 2..31 (only the first 32 bytes, even for big packets).
pub fn stamp(bytes: &mut [u8], magic: u32) {
    bytes[3] = ((magic >> 24) & 0xFF) as u8;
    bytes[2] = ((magic >> 16) & 0xFF) as u8;
    bytes[1] = 0;
    let mut sum: u8 = 0;
    for &b in &bytes[2..PACKAGE_SIZE] {
        sum = sum.wrapping_add(b);
    }
    bytes[1] = sum;
}

/// `ProcessReceivedDataEx`: validate magic + checksum of the first 32 bytes of a
/// received package, marking it `Invalid` on failure, then zero bytes 1..3.
///
/// Reproduces the C# comparison exactly: `int magic = (b3 << 24) + (b2 << 16)` is
/// compared against `uint (MagicNumber & 0xFFFF0000)`; C# promotes both to `long`,
/// so a magic number with bit 31 set can never validate (sign mismatch).
pub fn validate_received(buf: &mut [u8], magic: u32) -> bool {
    let mut ok = true;
    let m: i32 = ((buf[3] as i32) << 24).wrapping_add((buf[2] as i32) << 16);
    if (m as i64) != ((magic & 0xFFFF_0000) as i64) {
        buf[0] = ptype::Invalid as u8;
        ok = false;
    }
    let mut sum: u8 = 0;
    for &b in &buf[2..PACKAGE_SIZE] {
        sum = sum.wrapping_add(b);
    }
    if buf[1] != sum {
        buf[0] = ptype::Invalid as u8;
        ok = false;
    }
    buf[1] = 0;
    buf[2] = 0;
    buf[3] = 0;
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets() {
        let mut d = Data::new(ptype::Mouse);
        d.set_id(0x01020304);
        d.set_src(5);
        d.set_des(ID_ALL);
        d.set_md_x(100);
        d.set_md_y(-1);
        d.set_md_wheel(120);
        d.set_md_flags(wm::WM_MOUSEWHEEL);
        assert_eq!(&d.buf[0..4], &[123, 0, 0, 0]);
        assert_eq!(&d.buf[4..8], &[4, 3, 2, 1]);
        assert_eq!(d.buf[8], 5);
        assert_eq!(d.buf[12], 255);
        assert_eq!(&d.buf[16..20], &[100, 0, 0, 0]);
        assert_eq!(&d.buf[20..24], &[0xff; 4]);
        assert_eq!(d.kd_vk(), 120); // union overlap with WheelDelta
        assert_eq!(d.kd_flags(), wm::WM_MOUSEWHEEL);
        assert_eq!(d.wire_bytes().unwrap().len(), 32);
    }

    #[test]
    fn name_roundtrip() {
        let mut d = Data::new(ptype::Hello);
        d.set_machine_name("MY-PC");
        assert_eq!(&d.buf[32..37], b"MY-PC");
        assert!(d.buf[37..64].iter().all(|&b| b == b' '));
        assert_eq!(d.machine_name(), "MY-PC");
        d.set_machine_name("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789");
        assert_eq!(d.machine_name(), "ABCDEFGHIJKLMNOPQRSTUVWXYZ012345");
        assert_eq!(d.wire_bytes().unwrap().len(), 64);
    }

    #[test]
    fn big_rule() {
        assert_eq!(is_big_package(0), None);
        for t in [ptype::Hello, ptype::Heartbeat, ptype::Handshake, ptype::HandshakeAck, ptype::ClipboardText, ptype::Matrix | 6, ptype::Invalid, ptype::Error] {
            assert_eq!(is_big_package(t), Some(true), "{t}");
        }
        for t in [ptype::Mouse, ptype::Keyboard, ptype::HideMouse, ptype::NextMachine, ptype::ByeBye, ptype::Heartbeat_ex_l2] {
            assert_eq!(is_big_package(t), Some(false), "{t}");
        }
    }

    #[test]
    fn stamp_validate() {
        let magic = 0x1234_5678;
        let mut d = Data::new(ptype::Keyboard);
        d.set_kd_vk(0x41);
        let mut b = d.buf;
        stamp(&mut b, magic);
        assert_eq!(b[3], 0x12);
        assert_eq!(b[2], 0x34);
        let mut c = b;
        assert!(validate_received(&mut c, magic));
        assert_eq!(c[0], ptype::Keyboard as u8);
        assert_eq!(&c[1..4], &[0, 0, 0]);
        let mut c = b;
        c[20] ^= 1;
        assert!(!validate_received(&mut c, magic));
        assert_eq!(c[0], 0xFF);
        let mut c = b;
        assert!(!validate_received(&mut c, 0x1235_5678));
        // bit 31 quirk
        let mut b2 = d.buf;
        stamp(&mut b2, 0x8100_0000);
        assert!(!validate_received(&mut b2, 0x8100_0000));
    }
}
