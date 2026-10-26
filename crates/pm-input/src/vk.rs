//! Windows virtual-key codes and the macOS <-> Windows key translation tables.
//!
//! The MWB wire protocol carries Windows `VK_*` codes, so every backend speaks
//! VK. macOS keycodes are the `kVK_*` values from Carbon `Events.h`; they are
//! *positional* (ANSI US layout), which is also how Windows VK codes behave for
//! letters/digits/OEM keys on a US layout, so the mapping is position-for-position.
//!
//! Modifier convention (default): Command <-> Windows key (`VK_LWIN`/`VK_RWIN`),
//! Option <-> Alt (`VK_LMENU`/`VK_RMENU`), Control <-> Control, Shift <-> Shift.
//! Many mac<->Windows users prefer Command to act as Ctrl; apply
//! [`swap_cmd_ctrl`] / [`swap_cmd_ctrl_event`] in the engine for that.
//!
//! Layout caveats:
//! - ISO keyboards: macOS reports the key left of `1` as `kVK_ISO_Section` and the
//!   extra key next to left Shift as `kVK_ANSI_Grave` (Apple swaps them on some
//!   ISO hardware). We map `kVK_ANSI_Grave <-> VK_OEM_3` and
//!   `kVK_ISO_Section <-> VK_OEM_102`, which is right for ANSI keyboards.
//! - The mac keypad "Clear" key sits where Num Lock is, so it maps to `VK_NUMLOCK`
//!   (and `VK_CLEAR` maps back to it as well).
//! - Apple F13/F14/F15 sit where Print Screen/Scroll Lock/Pause are; they map to
//!   `VK_F13..F15`, and `VK_SNAPSHOT`/`VK_SCROLL`/`VK_PAUSE` map one-way onto them.
//! - `kVK_Function` (fn) has no Windows equivalent and is not mapped.

#![allow(dead_code)]

use crate::KeyEvent;

// ---------------------------------------------------------------------------
// Windows VK codes (WinUser.h)
// ---------------------------------------------------------------------------

pub const VK_LBUTTON: u32 = 0x01;
pub const VK_RBUTTON: u32 = 0x02;
pub const VK_CANCEL: u32 = 0x03;
pub const VK_MBUTTON: u32 = 0x04;
pub const VK_XBUTTON1: u32 = 0x05;
pub const VK_XBUTTON2: u32 = 0x06;
pub const VK_BACK: u32 = 0x08;
pub const VK_TAB: u32 = 0x09;
pub const VK_CLEAR: u32 = 0x0C;
pub const VK_RETURN: u32 = 0x0D;
pub const VK_SHIFT: u32 = 0x10;
pub const VK_CONTROL: u32 = 0x11;
pub const VK_MENU: u32 = 0x12;
pub const VK_PAUSE: u32 = 0x13;
pub const VK_CAPITAL: u32 = 0x14;
pub const VK_KANA: u32 = 0x15;
pub const VK_HANGUL: u32 = 0x15;
pub const VK_IME_ON: u32 = 0x16;
pub const VK_JUNJA: u32 = 0x17;
pub const VK_FINAL: u32 = 0x18;
pub const VK_HANJA: u32 = 0x19;
pub const VK_KANJI: u32 = 0x19;
pub const VK_IME_OFF: u32 = 0x1A;
pub const VK_ESCAPE: u32 = 0x1B;
pub const VK_CONVERT: u32 = 0x1C;
pub const VK_NONCONVERT: u32 = 0x1D;
pub const VK_ACCEPT: u32 = 0x1E;
pub const VK_MODECHANGE: u32 = 0x1F;
pub const VK_SPACE: u32 = 0x20;
pub const VK_PRIOR: u32 = 0x21;
pub const VK_NEXT: u32 = 0x22;
pub const VK_END: u32 = 0x23;
pub const VK_HOME: u32 = 0x24;
pub const VK_LEFT: u32 = 0x25;
pub const VK_UP: u32 = 0x26;
pub const VK_RIGHT: u32 = 0x27;
pub const VK_DOWN: u32 = 0x28;
pub const VK_SELECT: u32 = 0x29;
pub const VK_PRINT: u32 = 0x2A;
pub const VK_EXECUTE: u32 = 0x2B;
pub const VK_SNAPSHOT: u32 = 0x2C;
pub const VK_INSERT: u32 = 0x2D;
pub const VK_DELETE: u32 = 0x2E;
pub const VK_HELP: u32 = 0x2F;
// '0'..'9' = 0x30..0x39, 'A'..'Z' = 0x41..0x5A
pub const VK_0: u32 = 0x30;
pub const VK_1: u32 = 0x31;
pub const VK_2: u32 = 0x32;
pub const VK_3: u32 = 0x33;
pub const VK_4: u32 = 0x34;
pub const VK_5: u32 = 0x35;
pub const VK_6: u32 = 0x36;
pub const VK_7: u32 = 0x37;
pub const VK_8: u32 = 0x38;
pub const VK_9: u32 = 0x39;
pub const VK_A: u32 = 0x41;
pub const VK_B: u32 = 0x42;
pub const VK_C: u32 = 0x43;
pub const VK_D: u32 = 0x44;
pub const VK_E: u32 = 0x45;
pub const VK_F: u32 = 0x46;
pub const VK_G: u32 = 0x47;
pub const VK_H: u32 = 0x48;
pub const VK_I: u32 = 0x49;
pub const VK_J: u32 = 0x4A;
pub const VK_K: u32 = 0x4B;
pub const VK_L: u32 = 0x4C;
pub const VK_M: u32 = 0x4D;
pub const VK_N: u32 = 0x4E;
pub const VK_O: u32 = 0x4F;
pub const VK_P: u32 = 0x50;
pub const VK_Q: u32 = 0x51;
pub const VK_R: u32 = 0x52;
pub const VK_S: u32 = 0x53;
pub const VK_T: u32 = 0x54;
pub const VK_U: u32 = 0x55;
pub const VK_V: u32 = 0x56;
pub const VK_W: u32 = 0x57;
pub const VK_X: u32 = 0x58;
pub const VK_Y: u32 = 0x59;
pub const VK_Z: u32 = 0x5A;
pub const VK_LWIN: u32 = 0x5B;
pub const VK_RWIN: u32 = 0x5C;
pub const VK_APPS: u32 = 0x5D;
pub const VK_SLEEP: u32 = 0x5F;
pub const VK_NUMPAD0: u32 = 0x60;
pub const VK_NUMPAD1: u32 = 0x61;
pub const VK_NUMPAD2: u32 = 0x62;
pub const VK_NUMPAD3: u32 = 0x63;
pub const VK_NUMPAD4: u32 = 0x64;
pub const VK_NUMPAD5: u32 = 0x65;
pub const VK_NUMPAD6: u32 = 0x66;
pub const VK_NUMPAD7: u32 = 0x67;
pub const VK_NUMPAD8: u32 = 0x68;
pub const VK_NUMPAD9: u32 = 0x69;
pub const VK_MULTIPLY: u32 = 0x6A;
pub const VK_ADD: u32 = 0x6B;
pub const VK_SEPARATOR: u32 = 0x6C;
pub const VK_SUBTRACT: u32 = 0x6D;
pub const VK_DECIMAL: u32 = 0x6E;
pub const VK_DIVIDE: u32 = 0x6F;
pub const VK_F1: u32 = 0x70;
pub const VK_F2: u32 = 0x71;
pub const VK_F3: u32 = 0x72;
pub const VK_F4: u32 = 0x73;
pub const VK_F5: u32 = 0x74;
pub const VK_F6: u32 = 0x75;
pub const VK_F7: u32 = 0x76;
pub const VK_F8: u32 = 0x77;
pub const VK_F9: u32 = 0x78;
pub const VK_F10: u32 = 0x79;
pub const VK_F11: u32 = 0x7A;
pub const VK_F12: u32 = 0x7B;
pub const VK_F13: u32 = 0x7C;
pub const VK_F14: u32 = 0x7D;
pub const VK_F15: u32 = 0x7E;
pub const VK_F16: u32 = 0x7F;
pub const VK_F17: u32 = 0x80;
pub const VK_F18: u32 = 0x81;
pub const VK_F19: u32 = 0x82;
pub const VK_F20: u32 = 0x83;
pub const VK_F21: u32 = 0x84;
pub const VK_F22: u32 = 0x85;
pub const VK_F23: u32 = 0x86;
pub const VK_F24: u32 = 0x87;
pub const VK_NUMLOCK: u32 = 0x90;
pub const VK_SCROLL: u32 = 0x91;
pub const VK_OEM_NEC_EQUAL: u32 = 0x92;
pub const VK_LSHIFT: u32 = 0xA0;
pub const VK_RSHIFT: u32 = 0xA1;
pub const VK_LCONTROL: u32 = 0xA2;
pub const VK_RCONTROL: u32 = 0xA3;
pub const VK_LMENU: u32 = 0xA4;
pub const VK_RMENU: u32 = 0xA5;
pub const VK_BROWSER_BACK: u32 = 0xA6;
pub const VK_BROWSER_FORWARD: u32 = 0xA7;
pub const VK_BROWSER_REFRESH: u32 = 0xA8;
pub const VK_BROWSER_STOP: u32 = 0xA9;
pub const VK_BROWSER_SEARCH: u32 = 0xAA;
pub const VK_BROWSER_FAVORITES: u32 = 0xAB;
pub const VK_BROWSER_HOME: u32 = 0xAC;
pub const VK_VOLUME_MUTE: u32 = 0xAD;
pub const VK_VOLUME_DOWN: u32 = 0xAE;
pub const VK_VOLUME_UP: u32 = 0xAF;
pub const VK_MEDIA_NEXT_TRACK: u32 = 0xB0;
pub const VK_MEDIA_PREV_TRACK: u32 = 0xB1;
pub const VK_MEDIA_STOP: u32 = 0xB2;
pub const VK_MEDIA_PLAY_PAUSE: u32 = 0xB3;
pub const VK_LAUNCH_MAIL: u32 = 0xB4;
pub const VK_LAUNCH_MEDIA_SELECT: u32 = 0xB5;
pub const VK_LAUNCH_APP1: u32 = 0xB6;
pub const VK_LAUNCH_APP2: u32 = 0xB7;
pub const VK_OEM_1: u32 = 0xBA; // ;:
pub const VK_OEM_PLUS: u32 = 0xBB; // =+
pub const VK_OEM_COMMA: u32 = 0xBC; // ,<
pub const VK_OEM_MINUS: u32 = 0xBD; // -_
pub const VK_OEM_PERIOD: u32 = 0xBE; // .>
pub const VK_OEM_2: u32 = 0xBF; // /?
pub const VK_OEM_3: u32 = 0xC0; // `~
pub const VK_ABNT_C1: u32 = 0xC1;
pub const VK_ABNT_C2: u32 = 0xC2;
pub const VK_OEM_4: u32 = 0xDB; // [{
pub const VK_OEM_5: u32 = 0xDC; // \|
pub const VK_OEM_6: u32 = 0xDD; // ]}
pub const VK_OEM_7: u32 = 0xDE; // '"
pub const VK_OEM_8: u32 = 0xDF;
pub const VK_OEM_102: u32 = 0xE2; // ISO <> key
pub const VK_PROCESSKEY: u32 = 0xE5;
pub const VK_PACKET: u32 = 0xE7;
pub const VK_ATTN: u32 = 0xF6;
pub const VK_CRSEL: u32 = 0xF7;
pub const VK_EXSEL: u32 = 0xF8;
pub const VK_EREOF: u32 = 0xF9;
pub const VK_PLAY: u32 = 0xFA;
pub const VK_ZOOM: u32 = 0xFB;
pub const VK_NONAME: u32 = 0xFC;
pub const VK_PA1: u32 = 0xFD;
pub const VK_OEM_CLEAR: u32 = 0xFE;

// ---------------------------------------------------------------------------
// macOS virtual keycodes (Carbon HIToolbox/Events.h, kVK_*)
// ---------------------------------------------------------------------------

pub mod mac {
    pub const ANSI_A: u16 = 0x00;
    pub const ANSI_S: u16 = 0x01;
    pub const ANSI_D: u16 = 0x02;
    pub const ANSI_F: u16 = 0x03;
    pub const ANSI_H: u16 = 0x04;
    pub const ANSI_G: u16 = 0x05;
    pub const ANSI_Z: u16 = 0x06;
    pub const ANSI_X: u16 = 0x07;
    pub const ANSI_C: u16 = 0x08;
    pub const ANSI_V: u16 = 0x09;
    pub const ISO_SECTION: u16 = 0x0A;
    pub const ANSI_B: u16 = 0x0B;
    pub const ANSI_Q: u16 = 0x0C;
    pub const ANSI_W: u16 = 0x0D;
    pub const ANSI_E: u16 = 0x0E;
    pub const ANSI_R: u16 = 0x0F;
    pub const ANSI_Y: u16 = 0x10;
    pub const ANSI_T: u16 = 0x11;
    pub const ANSI_1: u16 = 0x12;
    pub const ANSI_2: u16 = 0x13;
    pub const ANSI_3: u16 = 0x14;
    pub const ANSI_4: u16 = 0x15;
    pub const ANSI_6: u16 = 0x16;
    pub const ANSI_5: u16 = 0x17;
    pub const ANSI_EQUAL: u16 = 0x18;
    pub const ANSI_9: u16 = 0x19;
    pub const ANSI_7: u16 = 0x1A;
    pub const ANSI_MINUS: u16 = 0x1B;
    pub const ANSI_8: u16 = 0x1C;
    pub const ANSI_0: u16 = 0x1D;
    pub const ANSI_RIGHT_BRACKET: u16 = 0x1E;
    pub const ANSI_O: u16 = 0x1F;
    pub const ANSI_U: u16 = 0x20;
    pub const ANSI_LEFT_BRACKET: u16 = 0x21;
    pub const ANSI_I: u16 = 0x22;
    pub const ANSI_P: u16 = 0x23;
    pub const RETURN: u16 = 0x24;
    pub const ANSI_L: u16 = 0x25;
    pub const ANSI_J: u16 = 0x26;
    pub const ANSI_QUOTE: u16 = 0x27;
    pub const ANSI_K: u16 = 0x28;
    pub const ANSI_SEMICOLON: u16 = 0x29;
    pub const ANSI_BACKSLASH: u16 = 0x2A;
    pub const ANSI_COMMA: u16 = 0x2B;
    pub const ANSI_SLASH: u16 = 0x2C;
    pub const ANSI_N: u16 = 0x2D;
    pub const ANSI_M: u16 = 0x2E;
    pub const ANSI_PERIOD: u16 = 0x2F;
    pub const TAB: u16 = 0x30;
    pub const SPACE: u16 = 0x31;
    pub const ANSI_GRAVE: u16 = 0x32;
    pub const DELETE: u16 = 0x33;
    pub const ESCAPE: u16 = 0x35;
    pub const RIGHT_COMMAND: u16 = 0x36;
    pub const COMMAND: u16 = 0x37;
    pub const SHIFT: u16 = 0x38;
    pub const CAPS_LOCK: u16 = 0x39;
    pub const OPTION: u16 = 0x3A;
    pub const CONTROL: u16 = 0x3B;
    pub const RIGHT_SHIFT: u16 = 0x3C;
    pub const RIGHT_OPTION: u16 = 0x3D;
    pub const RIGHT_CONTROL: u16 = 0x3E;
    pub const FUNCTION: u16 = 0x3F;
    pub const F17: u16 = 0x40;
    pub const KEYPAD_DECIMAL: u16 = 0x41;
    pub const KEYPAD_MULTIPLY: u16 = 0x43;
    pub const KEYPAD_PLUS: u16 = 0x45;
    pub const KEYPAD_CLEAR: u16 = 0x47;
    pub const VOLUME_UP: u16 = 0x48;
    pub const VOLUME_DOWN: u16 = 0x49;
    pub const MUTE: u16 = 0x4A;
    pub const KEYPAD_DIVIDE: u16 = 0x4B;
    pub const KEYPAD_ENTER: u16 = 0x4C;
    pub const KEYPAD_MINUS: u16 = 0x4E;
    pub const F18: u16 = 0x4F;
    pub const F19: u16 = 0x50;
    pub const KEYPAD_EQUALS: u16 = 0x51;
    pub const KEYPAD_0: u16 = 0x52;
    pub const KEYPAD_1: u16 = 0x53;
    pub const KEYPAD_2: u16 = 0x54;
    pub const KEYPAD_3: u16 = 0x55;
    pub const KEYPAD_4: u16 = 0x56;
    pub const KEYPAD_5: u16 = 0x57;
    pub const KEYPAD_6: u16 = 0x58;
    pub const KEYPAD_7: u16 = 0x59;
    pub const F20: u16 = 0x5A;
    pub const KEYPAD_8: u16 = 0x5B;
    pub const KEYPAD_9: u16 = 0x5C;
    pub const JIS_YEN: u16 = 0x5D;
    pub const JIS_UNDERSCORE: u16 = 0x5E;
    pub const JIS_KEYPAD_COMMA: u16 = 0x5F;
    pub const F5: u16 = 0x60;
    pub const F6: u16 = 0x61;
    pub const F7: u16 = 0x62;
    pub const F3: u16 = 0x63;
    pub const F8: u16 = 0x64;
    pub const F9: u16 = 0x65;
    pub const JIS_EISU: u16 = 0x66;
    pub const F11: u16 = 0x67;
    pub const JIS_KANA: u16 = 0x68;
    pub const F13: u16 = 0x69;
    pub const F16: u16 = 0x6A;
    pub const F14: u16 = 0x6B;
    pub const F10: u16 = 0x6D;
    pub const CONTEXTUAL_MENU: u16 = 0x6E;
    pub const F12: u16 = 0x6F;
    pub const F15: u16 = 0x71;
    pub const HELP: u16 = 0x72;
    pub const HOME: u16 = 0x73;
    pub const PAGE_UP: u16 = 0x74;
    pub const FORWARD_DELETE: u16 = 0x75;
    pub const F4: u16 = 0x76;
    pub const END: u16 = 0x77;
    pub const F2: u16 = 0x78;
    pub const PAGE_DOWN: u16 = 0x79;
    pub const F1: u16 = 0x7A;
    pub const LEFT_ARROW: u16 = 0x7B;
    pub const RIGHT_ARROW: u16 = 0x7C;
    pub const DOWN_ARROW: u16 = 0x7D;
    pub const UP_ARROW: u16 = 0x7E;
}

/// Bidirectional table: (mac keycode, VK, extended bit when this mac key is pressed).
/// Every row round-trips: `mac_to_vk(m) == (vk, ext)` and `vk_to_mac(vk, ext) == m`.
const TABLE: &[(u16, u32, bool)] = &[
    (mac::ANSI_A, VK_A, false),
    (mac::ANSI_S, VK_S, false),
    (mac::ANSI_D, VK_D, false),
    (mac::ANSI_F, VK_F, false),
    (mac::ANSI_H, VK_H, false),
    (mac::ANSI_G, VK_G, false),
    (mac::ANSI_Z, VK_Z, false),
    (mac::ANSI_X, VK_X, false),
    (mac::ANSI_C, VK_C, false),
    (mac::ANSI_V, VK_V, false),
    (mac::ISO_SECTION, VK_OEM_102, false),
    (mac::ANSI_B, VK_B, false),
    (mac::ANSI_Q, VK_Q, false),
    (mac::ANSI_W, VK_W, false),
    (mac::ANSI_E, VK_E, false),
    (mac::ANSI_R, VK_R, false),
    (mac::ANSI_Y, VK_Y, false),
    (mac::ANSI_T, VK_T, false),
    (mac::ANSI_1, VK_1, false),
    (mac::ANSI_2, VK_2, false),
    (mac::ANSI_3, VK_3, false),
    (mac::ANSI_4, VK_4, false),
    (mac::ANSI_6, VK_6, false),
    (mac::ANSI_5, VK_5, false),
    (mac::ANSI_EQUAL, VK_OEM_PLUS, false),
    (mac::ANSI_9, VK_9, false),
    (mac::ANSI_7, VK_7, false),
    (mac::ANSI_MINUS, VK_OEM_MINUS, false),
    (mac::ANSI_8, VK_8, false),
    (mac::ANSI_0, VK_0, false),
    (mac::ANSI_RIGHT_BRACKET, VK_OEM_6, false),
    (mac::ANSI_O, VK_O, false),
    (mac::ANSI_U, VK_U, false),
    (mac::ANSI_LEFT_BRACKET, VK_OEM_4, false),
    (mac::ANSI_I, VK_I, false),
    (mac::ANSI_P, VK_P, false),
    (mac::RETURN, VK_RETURN, false),
    (mac::ANSI_L, VK_L, false),
    (mac::ANSI_J, VK_J, false),
    (mac::ANSI_QUOTE, VK_OEM_7, false),
    (mac::ANSI_K, VK_K, false),
    (mac::ANSI_SEMICOLON, VK_OEM_1, false),
    (mac::ANSI_BACKSLASH, VK_OEM_5, false),
    (mac::ANSI_COMMA, VK_OEM_COMMA, false),
    (mac::ANSI_SLASH, VK_OEM_2, false),
    (mac::ANSI_N, VK_N, false),
    (mac::ANSI_M, VK_M, false),
    (mac::ANSI_PERIOD, VK_OEM_PERIOD, false),
    (mac::TAB, VK_TAB, false),
    (mac::SPACE, VK_SPACE, false),
    (mac::ANSI_GRAVE, VK_OEM_3, false),
    (mac::DELETE, VK_BACK, false),
    (mac::ESCAPE, VK_ESCAPE, false),
    (mac::RIGHT_COMMAND, VK_RWIN, true),
    (mac::COMMAND, VK_LWIN, true),
    (mac::SHIFT, VK_LSHIFT, false),
    (mac::CAPS_LOCK, VK_CAPITAL, false),
    (mac::OPTION, VK_LMENU, false),
    (mac::CONTROL, VK_LCONTROL, false),
    (mac::RIGHT_SHIFT, VK_RSHIFT, false),
    (mac::RIGHT_OPTION, VK_RMENU, true),
    (mac::RIGHT_CONTROL, VK_RCONTROL, true),
    (mac::F17, VK_F17, false),
    (mac::KEYPAD_DECIMAL, VK_DECIMAL, false),
    (mac::KEYPAD_MULTIPLY, VK_MULTIPLY, false),
    (mac::KEYPAD_PLUS, VK_ADD, false),
    (mac::KEYPAD_CLEAR, VK_NUMLOCK, true),
    (mac::VOLUME_UP, VK_VOLUME_UP, true),
    (mac::VOLUME_DOWN, VK_VOLUME_DOWN, true),
    (mac::MUTE, VK_VOLUME_MUTE, true),
    (mac::KEYPAD_DIVIDE, VK_DIVIDE, true),
    (mac::KEYPAD_ENTER, VK_RETURN, true),
    (mac::KEYPAD_MINUS, VK_SUBTRACT, false),
    (mac::F18, VK_F18, false),
    (mac::F19, VK_F19, false),
    (mac::KEYPAD_EQUALS, VK_OEM_NEC_EQUAL, false),
    (mac::KEYPAD_0, VK_NUMPAD0, false),
    (mac::KEYPAD_1, VK_NUMPAD1, false),
    (mac::KEYPAD_2, VK_NUMPAD2, false),
    (mac::KEYPAD_3, VK_NUMPAD3, false),
    (mac::KEYPAD_4, VK_NUMPAD4, false),
    (mac::KEYPAD_5, VK_NUMPAD5, false),
    (mac::KEYPAD_6, VK_NUMPAD6, false),
    (mac::KEYPAD_7, VK_NUMPAD7, false),
    (mac::F20, VK_F20, false),
    (mac::KEYPAD_8, VK_NUMPAD8, false),
    (mac::KEYPAD_9, VK_NUMPAD9, false),
    (mac::JIS_YEN, VK_OEM_8, false),
    (mac::JIS_UNDERSCORE, VK_ABNT_C1, false),
    (mac::JIS_KEYPAD_COMMA, VK_SEPARATOR, false),
    (mac::F5, VK_F5, false),
    (mac::F6, VK_F6, false),
    (mac::F7, VK_F7, false),
    (mac::F3, VK_F3, false),
    (mac::F8, VK_F8, false),
    (mac::F9, VK_F9, false),
    (mac::JIS_EISU, VK_NONCONVERT, false),
    (mac::F11, VK_F11, false),
    (mac::JIS_KANA, VK_KANA, false),
    (mac::F13, VK_F13, false),
    (mac::F16, VK_F16, false),
    (mac::F14, VK_F14, false),
    (mac::F10, VK_F10, false),
    (mac::CONTEXTUAL_MENU, VK_APPS, true),
    (mac::F12, VK_F12, false),
    (mac::F15, VK_F15, false),
    (mac::HELP, VK_INSERT, true),
    (mac::HOME, VK_HOME, true),
    (mac::PAGE_UP, VK_PRIOR, true),
    (mac::FORWARD_DELETE, VK_DELETE, true),
    (mac::F4, VK_F4, false),
    (mac::END, VK_END, true),
    (mac::F2, VK_F2, false),
    (mac::PAGE_DOWN, VK_NEXT, true),
    (mac::F1, VK_F1, false),
    (mac::LEFT_ARROW, VK_LEFT, true),
    (mac::RIGHT_ARROW, VK_RIGHT, true),
    (mac::DOWN_ARROW, VK_DOWN, true),
    (mac::UP_ARROW, VK_UP, true),
];

/// One-way aliases (VK -> mac) for VKs that have no dedicated mac key.
/// `None` for the extended bit means "regardless of extended".
const VK_ALIASES: &[(u32, Option<bool>, u16)] = &[
    (VK_SHIFT, None, mac::SHIFT),
    (VK_CONTROL, Some(false), mac::CONTROL),
    (VK_CONTROL, Some(true), mac::RIGHT_CONTROL),
    (VK_MENU, Some(false), mac::OPTION),
    (VK_MENU, Some(true), mac::RIGHT_OPTION),
    (VK_CLEAR, None, mac::KEYPAD_CLEAR),
    (VK_SNAPSHOT, None, mac::F13),
    (VK_SCROLL, None, mac::F14),
    (VK_PAUSE, None, mac::F15),
    (VK_HELP, None, mac::HELP),
    (VK_IME_OFF, None, mac::JIS_EISU),
    (VK_IME_ON, None, mac::JIS_KANA),
];

/// Translate a macOS virtual keycode to `(VK, extended)`.
pub fn mac_to_vk(keycode: u16) -> Option<(u32, bool)> {
    TABLE
        .iter()
        .find(|(m, _, _)| *m == keycode)
        .map(|&(_, vk, ext)| (vk, ext))
}

/// Translate a Windows VK (+ extended bit) to a macOS virtual keycode.
///
/// The extended bit disambiguates `VK_RETURN` (main vs. keypad Enter) and the
/// generic `VK_CONTROL`/`VK_MENU`. For every other key it is ignored.
pub fn vk_to_mac(vk: u32, extended: bool) -> Option<u16> {
    // Exact match first (handles keypad Enter vs Return).
    if let Some(&(m, _, _)) = TABLE.iter().find(|(_, v, e)| *v == vk && *e == extended) {
        return Some(m);
    }
    if let Some(&(_, _, m)) = VK_ALIASES
        .iter()
        .find(|(v, e, _)| *v == vk && e.is_none_or(|e| e == extended))
    {
        return Some(m);
    }
    // Ignore the extended bit for everything else (a sender may not set it).
    if vk == VK_RETURN {
        return Some(if extended { mac::KEYPAD_ENTER } else { mac::RETURN });
    }
    TABLE.iter().find(|(_, v, _)| *v == vk).map(|&(m, _, _)| m)
}

/// Convenience: build a [`KeyEvent`] from a mac keycode.
pub fn mac_key_event(keycode: u16, down: bool) -> Option<KeyEvent> {
    mac_to_vk(keycode).map(|(vk, extended)| KeyEvent { vk, down, extended })
}

/// Whether `vk` is normally an "extended" key (E0-prefixed scan code) on Windows.
///
/// `VK_RETURN` returns false here (main Enter); keypad Enter is `VK_RETURN` with
/// the extended bit set, which only the event itself can tell you.
pub fn is_extended_vk(vk: u32) -> bool {
    matches!(
        vk,
        VK_CANCEL
            | VK_PRIOR
            | VK_NEXT
            | VK_END
            | VK_HOME
            | VK_LEFT
            | VK_UP
            | VK_RIGHT
            | VK_DOWN
            | VK_SNAPSHOT
            | VK_INSERT
            | VK_DELETE
            | VK_LWIN
            | VK_RWIN
            | VK_APPS
            | VK_SLEEP
            | VK_DIVIDE
            | VK_NUMLOCK
            | VK_RCONTROL
            | VK_RMENU
            | VK_BROWSER_BACK
            | VK_BROWSER_FORWARD
            | VK_BROWSER_REFRESH
            | VK_BROWSER_STOP
            | VK_BROWSER_SEARCH
            | VK_BROWSER_FAVORITES
            | VK_BROWSER_HOME
            | VK_VOLUME_MUTE
            | VK_VOLUME_DOWN
            | VK_VOLUME_UP
            | VK_MEDIA_NEXT_TRACK
            | VK_MEDIA_PREV_TRACK
            | VK_MEDIA_STOP
            | VK_MEDIA_PLAY_PAUSE
            | VK_LAUNCH_MAIL
            | VK_LAUNCH_MEDIA_SELECT
            | VK_LAUNCH_APP1
            | VK_LAUNCH_APP2
    )
}

/// Whether `vk` is a modifier key (generic or sided).
pub fn is_modifier_vk(vk: u32) -> bool {
    matches!(
        vk,
        VK_SHIFT
            | VK_CONTROL
            | VK_MENU
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_LMENU
            | VK_RMENU
            | VK_LWIN
            | VK_RWIN
    )
}

/// Swap Command (Windows key) and Control: `VK_LWIN <-> VK_LCONTROL`,
/// `VK_RWIN <-> VK_RCONTROL`, generic `VK_CONTROL -> VK_LWIN`. Other VKs unchanged.
///
/// Apply to captured events on a Mac before sending (so Cmd+C arrives as Ctrl+C),
/// and/or to received events before injecting on a Mac (so Ctrl+C acts as Cmd+C).
/// The swap is its own inverse except for generic `VK_CONTROL`.
pub fn swap_cmd_ctrl(vk: u32) -> u32 {
    match vk {
        VK_LWIN => VK_LCONTROL,
        VK_LCONTROL => VK_LWIN,
        VK_RWIN => VK_RCONTROL,
        VK_RCONTROL => VK_RWIN,
        VK_CONTROL => VK_LWIN,
        other => other,
    }
}

/// [`swap_cmd_ctrl`] applied to a whole event, fixing up the extended bit.
pub fn swap_cmd_ctrl_event(ev: KeyEvent) -> KeyEvent {
    let vk = swap_cmd_ctrl(ev.vk);
    if vk == ev.vk {
        return ev;
    }
    KeyEvent { vk, down: ev.down, extended: is_extended_vk(vk) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_no_duplicates() {
        for (i, a) in TABLE.iter().enumerate() {
            for b in &TABLE[i + 1..] {
                assert_ne!(a.0, b.0, "duplicate mac keycode {:#x}", a.0);
                assert!(!(a.1 == b.1 && a.2 == b.2), "duplicate vk {:#x}/{}", a.1, a.2);
            }
        }
    }

    #[test]
    fn mac_round_trip() {
        for &(m, vk, ext) in TABLE {
            assert_eq!(mac_to_vk(m), Some((vk, ext)));
            assert_eq!(vk_to_mac(vk, ext), Some(m), "vk {vk:#x} ext {ext}");
        }
    }

    #[test]
    fn extended_flag_matches_windows_convention() {
        for &(_, vk, ext) in TABLE {
            if vk == VK_RETURN {
                continue;
            }
            assert_eq!(is_extended_vk(vk), ext, "vk {vk:#x}");
        }
    }

    #[test]
    fn return_vs_keypad_enter() {
        assert_eq!(vk_to_mac(VK_RETURN, false), Some(mac::RETURN));
        assert_eq!(vk_to_mac(VK_RETURN, true), Some(mac::KEYPAD_ENTER));
        assert_eq!(mac_to_vk(mac::KEYPAD_ENTER), Some((VK_RETURN, true)));
    }

    #[test]
    fn modifiers() {
        assert_eq!(mac_to_vk(mac::COMMAND), Some((VK_LWIN, true)));
        assert_eq!(mac_to_vk(mac::RIGHT_COMMAND), Some((VK_RWIN, true)));
        assert_eq!(mac_to_vk(mac::OPTION), Some((VK_LMENU, false)));
        assert_eq!(mac_to_vk(mac::RIGHT_OPTION), Some((VK_RMENU, true)));
        assert_eq!(mac_to_vk(mac::CONTROL), Some((VK_LCONTROL, false)));
        assert_eq!(mac_to_vk(mac::RIGHT_SHIFT), Some((VK_RSHIFT, false)));
        assert_eq!(mac_to_vk(mac::FUNCTION), None);
        // generic
        assert_eq!(vk_to_mac(VK_SHIFT, false), Some(mac::SHIFT));
        assert_eq!(vk_to_mac(VK_CONTROL, false), Some(mac::CONTROL));
        assert_eq!(vk_to_mac(VK_CONTROL, true), Some(mac::RIGHT_CONTROL));
        assert_eq!(vk_to_mac(VK_MENU, true), Some(mac::RIGHT_OPTION));
        // Sender forgot the extended bit
        assert_eq!(vk_to_mac(VK_LEFT, false), Some(mac::LEFT_ARROW));
        assert_eq!(vk_to_mac(VK_LWIN, false), Some(mac::COMMAND));
    }

    #[test]
    fn function_keys_complete() {
        for n in 0..20u32 {
            let vk = VK_F1 + n;
            let m = vk_to_mac(vk, false).unwrap_or_else(|| panic!("F{} unmapped", n + 1));
            assert_eq!(mac_to_vk(m), Some((vk, false)));
        }
        assert_eq!(vk_to_mac(VK_F21, false), None);
    }

    #[test]
    fn letters_and_digits_complete() {
        for vk in (VK_A..=VK_Z).chain(VK_0..=VK_9).chain(VK_NUMPAD0..=VK_NUMPAD9) {
            assert!(vk_to_mac(vk, false).is_some(), "vk {vk:#x}");
        }
    }

    #[test]
    fn swap() {
        for vk in [VK_LWIN, VK_RWIN, VK_LCONTROL, VK_RCONTROL, VK_A] {
            assert_eq!(swap_cmd_ctrl(swap_cmd_ctrl(vk)), vk);
        }
        let e = swap_cmd_ctrl_event(KeyEvent { vk: VK_LWIN, down: true, extended: true });
        assert_eq!(e, KeyEvent { vk: VK_LCONTROL, down: true, extended: false });
        let e = swap_cmd_ctrl_event(KeyEvent { vk: VK_RCONTROL, down: false, extended: true });
        assert_eq!(e, KeyEvent { vk: VK_RWIN, down: false, extended: true });
        let a = KeyEvent { vk: VK_A, down: true, extended: false };
        assert_eq!(swap_cmd_ctrl_event(a), a);
    }
}
