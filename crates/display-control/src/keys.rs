//! Media-key decoding and key routing (pure logic shared by the macOS event
//! tap and the Windows keyboard hook).

use std::time::{Duration, Instant};

use crate::config::{Config, Mods};
use crate::ddc::Feature;

/// A brightness / volume key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKey {
    BrightnessUp,
    BrightnessDown,
    VolumeUp,
    VolumeDown,
    Mute,
}

impl MediaKey {
    pub fn is_brightness(self) -> bool {
        matches!(self, MediaKey::BrightnessUp | MediaKey::BrightnessDown)
    }
}

/// A decoded media-key transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaKeyEvent {
    pub key: MediaKey,
    pub down: bool,
    pub repeat: bool,
}

/// `NSEventTypeSystemDefined` / `kCGEventSystemDefined`.
pub const NS_SYSTEM_DEFINED: u32 = 14;
/// `NX_SUBTYPE_AUX_CONTROL_BUTTONS`.
pub const NX_SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;

// IOKit/hidsystem/ev_keymap.h
const NX_KEYTYPE_SOUND_UP: i64 = 0;
const NX_KEYTYPE_SOUND_DOWN: i64 = 1;
const NX_KEYTYPE_BRIGHTNESS_UP: i64 = 2;
const NX_KEYTYPE_BRIGHTNESS_DOWN: i64 = 3;
const NX_KEYTYPE_MUTE: i64 = 7;

/// Decode an `NSSystemDefined` event's `subtype` and `data1`.
///
/// `data1` = key code in bits 16–31, key flags in bits 0–15, where the
/// high byte of the flags is 0x0A for key-down and 0x0B for key-up and
/// bit 0 marks an auto-repeat.
pub fn parse_system_defined(subtype: i16, data1: i64) -> Option<MediaKeyEvent> {
    if subtype != NX_SUBTYPE_AUX_CONTROL_BUTTONS {
        return None;
    }
    let code = (data1 & 0xFFFF_0000) >> 16;
    let flags = data1 & 0xFFFF;
    let state = (flags & 0xFF00) >> 8;
    let down = match state {
        0x0A => true,
        0x0B => false,
        _ => return None,
    };
    let key = match code {
        NX_KEYTYPE_SOUND_UP => MediaKey::VolumeUp,
        NX_KEYTYPE_SOUND_DOWN => MediaKey::VolumeDown,
        NX_KEYTYPE_BRIGHTNESS_UP => MediaKey::BrightnessUp,
        NX_KEYTYPE_BRIGHTNESS_DOWN => MediaKey::BrightnessDown,
        NX_KEYTYPE_MUTE => MediaKey::Mute,
        _ => return None,
    };
    Some(MediaKeyEvent { key, down, repeat: flags & 1 != 0 })
}

/// Build `data1` for a media key (tests / diagnostics).
pub fn encode_system_defined(key: MediaKey, down: bool, repeat: bool) -> i64 {
    let code = match key {
        MediaKey::VolumeUp => NX_KEYTYPE_SOUND_UP,
        MediaKey::VolumeDown => NX_KEYTYPE_SOUND_DOWN,
        MediaKey::BrightnessUp => NX_KEYTYPE_BRIGHTNESS_UP,
        MediaKey::BrightnessDown => NX_KEYTYPE_BRIGHTNESS_DOWN,
        MediaKey::Mute => NX_KEYTYPE_MUTE,
    };
    (code << 16) | (if down { 0x0A00 } else { 0x0B00 }) | repeat as i64
}

/// Apple keyboards on recent Macs also send the brightness keys as plain
/// key events with these virtual key codes.
pub fn mac_keycode_media(keycode: u16) -> Option<MediaKey> {
    match keycode {
        144 => Some(MediaKey::BrightnessUp),
        145 => Some(MediaKey::BrightnessDown),
        0x48 => Some(MediaKey::VolumeUp),
        0x49 => Some(MediaKey::VolumeDown),
        0x4A => Some(MediaKey::Mute),
        _ => None,
    }
}

/// Windows `VK_VOLUME_*`.
pub fn windows_vk_media(vk: u32) -> Option<MediaKey> {
    match vk {
        0xAD => Some(MediaKey::Mute),
        0xAE => Some(MediaKey::VolumeDown),
        0xAF => Some(MediaKey::VolumeUp),
        _ => None,
    }
}

/// What to do with the target display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Change a feature by `steps` (negative = down) of `step_pct` percent.
    Adjust { feature: Feature, up: bool, fine: bool },
    ToggleMute,
}

impl Action {
    pub fn for_media_key(key: MediaKey, mods: Mods) -> Self {
        let fine = mods.alt && mods.shift;
        match key {
            MediaKey::BrightnessUp => Action::Adjust { feature: Feature::Brightness, up: true, fine },
            MediaKey::BrightnessDown => Action::Adjust { feature: Feature::Brightness, up: false, fine },
            MediaKey::VolumeUp => Action::Adjust { feature: Feature::Volume, up: true, fine },
            MediaKey::VolumeDown => Action::Adjust { feature: Feature::Volume, up: false, fine },
            MediaKey::Mute => Action::ToggleMute,
        }
    }

    pub fn feature(self) -> Feature {
        match self {
            Action::Adjust { feature, .. } => feature,
            Action::ToggleMute => Feature::Mute,
        }
    }
}

/// The hotkey (if any) a key-down of `vk` with `mods` triggers.
pub fn hotkey_action(cfg: &Config, vk: u32, mods: Mods) -> Option<Action> {
    let h = &cfg.hotkeys;
    let adj = |feature, up| Some(Action::Adjust { feature, up, fine: false });
    if h.brightness_up.matches(vk, mods) {
        adj(Feature::Brightness, true)
    } else if h.brightness_down.matches(vk, mods) {
        adj(Feature::Brightness, false)
    } else if h.contrast_up.matches(vk, mods) {
        adj(Feature::Contrast, true)
    } else if h.contrast_down.matches(vk, mods) {
        adj(Feature::Contrast, false)
    } else if h.volume_up.matches(vk, mods) {
        adj(Feature::Volume, true)
    } else if h.volume_down.matches(vk, mods) {
        adj(Feature::Volume, false)
    } else if h.mute.matches(vk, mods) {
        Some(Action::ToggleMute)
    } else {
        None
    }
}

/// New percentage after one step. A fine step is a quarter of `step`
/// (at least 1). Values snap to the step grid, so 47 + 6 → 48, not 53.
pub fn step_value(current: u8, step: u8, up: bool, fine: bool) -> u8 {
    let step = if fine { (step / 4).max(1) } else { step.max(1) } as i32;
    let cur = current.min(100) as i32;
    let next = if up { (cur / step + 1) * step } else { ((cur + step - 1) / step - 1) * step };
    next.clamp(0, 100) as u8
}

/// Where a media-key event came from (the same physical press can arrive
/// both as a system-defined event and as a key event).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    SystemDefined,
    KeyCode,
}

/// Remembers which key presses were taken over, so that their key-ups (and
/// the duplicate representation of the same press) are swallowed too and
/// acted on only once.
#[derive(Debug, Default)]
pub struct KeyRouter {
    /// Keys whose press we swallowed, by source.
    swallowed: Vec<(MediaKey, KeySource)>,
    last_act: Option<(MediaKey, KeySource, Instant)>,
}

/// Decision for one media-key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Let the system have it.
    Pass,
    /// Swallow it and perform the action.
    Act,
    /// Swallow it without acting (a key-up, or the twin of a handled press).
    Swallow,
}

const TWIN_WINDOW: Duration = Duration::from_millis(80);

impl KeyRouter {
    /// `take_over` = whether this key is taken over for the current target
    /// (computed by the caller only for key-downs).
    pub fn route(&mut self, ev: MediaKeyEvent, source: KeySource, take_over: impl FnOnce() -> bool, now: Instant) -> Route {
        if !ev.down {
            if let Some(i) = self.swallowed.iter().position(|&(k, s)| k == ev.key && s == source) {
                self.swallowed.remove(i);
                return Route::Swallow;
            }
            return Route::Pass;
        }
        // The other representation of a press we just acted on.
        if let Some((k, s, t)) = self.last_act {
            if k == ev.key && s != source && now.duration_since(t) < TWIN_WINDOW && !ev.repeat {
                self.remember(ev.key, source);
                self.last_act = None;
                return Route::Swallow;
            }
        }
        if !take_over() {
            return Route::Pass;
        }
        self.remember(ev.key, source);
        self.last_act = Some((ev.key, source, now));
        Route::Act
    }

    fn remember(&mut self, key: MediaKey, source: KeySource) {
        if !self.swallowed.contains(&(key, source)) {
            self.swallowed.push((key, source));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Hotkey;

    #[test]
    fn parses_system_defined() {
        // Brightness up, key down: 0x0002_0A00.
        assert_eq!(parse_system_defined(8, 0x0002_0A00), Some(MediaKeyEvent { key: MediaKey::BrightnessUp, down: true, repeat: false }));
        assert_eq!(parse_system_defined(8, 0x0003_0B00), Some(MediaKeyEvent { key: MediaKey::BrightnessDown, down: false, repeat: false }));
        assert_eq!(parse_system_defined(8, 0x0000_0A01), Some(MediaKeyEvent { key: MediaKey::VolumeUp, down: true, repeat: true }));
        assert_eq!(parse_system_defined(8, 0x0007_0A00).unwrap().key, MediaKey::Mute);
        assert_eq!(parse_system_defined(8, 0x0001_0A00).unwrap().key, MediaKey::VolumeDown);
        // Wrong subtype (e.g. 7 = power key / other), unknown key, bad state.
        assert_eq!(parse_system_defined(7, 0x0002_0A00), None);
        assert_eq!(parse_system_defined(8, 0x0010_0A00), None, "play/pause is not ours");
        assert_eq!(parse_system_defined(8, 0x0002_0C00), None);
        // Upper bits of data1 (sign extension on 64-bit) don't matter.
        assert!(parse_system_defined(8, 0x7FFF_0000_0002_0A00).is_some());
        for k in [MediaKey::BrightnessUp, MediaKey::BrightnessDown, MediaKey::VolumeUp, MediaKey::VolumeDown, MediaKey::Mute] {
            for down in [true, false] {
                assert_eq!(parse_system_defined(8, encode_system_defined(k, down, false)), Some(MediaKeyEvent { key: k, down, repeat: false }));
            }
        }
    }

    #[test]
    fn keycodes() {
        assert_eq!(mac_keycode_media(144), Some(MediaKey::BrightnessUp));
        assert_eq!(mac_keycode_media(145), Some(MediaKey::BrightnessDown));
        assert_eq!(mac_keycode_media(0), None);
        assert_eq!(windows_vk_media(0xAF), Some(MediaKey::VolumeUp));
        assert_eq!(windows_vk_media(0x41), None);
        assert!(MediaKey::BrightnessDown.is_brightness());
        assert!(!MediaKey::Mute.is_brightness());
    }

    #[test]
    fn actions() {
        let fine = Mods { alt: true, shift: true, ..Default::default() };
        assert_eq!(Action::for_media_key(MediaKey::BrightnessUp, fine), Action::Adjust { feature: Feature::Brightness, up: true, fine: true });
        assert_eq!(Action::for_media_key(MediaKey::VolumeDown, Mods::default()), Action::Adjust { feature: Feature::Volume, up: false, fine: false });
        assert_eq!(Action::for_media_key(MediaKey::Mute, Mods::default()).feature(), Feature::Mute);
        let mut cfg = Config::default();
        cfg.hotkeys.contrast_down = Hotkey::new(false, true, true, false, 0xDB);
        cfg.hotkeys.mute = Hotkey::new(true, false, false, true, 0x4D);
        let ca = Mods { ctrl: true, alt: true, ..Default::default() };
        assert_eq!(hotkey_action(&cfg, 0xDB, ca), Some(Action::Adjust { feature: Feature::Contrast, up: false, fine: false }));
        assert_eq!(hotkey_action(&cfg, 0x4D, Mods { win: true, shift: true, ..Default::default() }), Some(Action::ToggleMute));
        assert_eq!(hotkey_action(&cfg, 0xDB, Mods::default()), None);
    }

    #[test]
    fn stepping() {
        assert_eq!(step_value(50, 6, true, false), 54);
        assert_eq!(step_value(48, 6, true, false), 54);
        assert_eq!(step_value(48, 6, false, false), 42);
        assert_eq!(step_value(47, 6, false, false), 42);
        assert_eq!(step_value(47, 6, true, false), 48);
        assert_eq!(step_value(98, 6, true, false), 100);
        assert_eq!(step_value(100, 6, true, false), 100);
        assert_eq!(step_value(3, 6, false, false), 0);
        assert_eq!(step_value(0, 6, false, false), 0);
        assert_eq!(step_value(50, 6, true, true), 51, "fine = quarter step, min 1");
        assert_eq!(step_value(50, 20, true, true), 55);
        assert_eq!(step_value(50, 0, true, false), 51);
        assert_eq!(step_value(250, 10, false, false), 90);
    }

    #[test]
    fn router_swallows_ups_and_twins() {
        let t0 = Instant::now();
        let mut r = KeyRouter::default();
        let down = MediaKeyEvent { key: MediaKey::BrightnessUp, down: true, repeat: false };
        let up = MediaKeyEvent { down: false, ..down };
        // Not taken over: everything passes.
        assert_eq!(r.route(down, KeySource::SystemDefined, || false, t0), Route::Pass);
        assert_eq!(r.route(up, KeySource::SystemDefined, || unreachable!(), t0), Route::Pass);
        // Taken over: press acts, its twin keycode event is swallowed, both ups swallowed.
        assert_eq!(r.route(down, KeySource::SystemDefined, || true, t0), Route::Act);
        assert_eq!(r.route(down, KeySource::KeyCode, || true, t0 + Duration::from_millis(5)), Route::Swallow);
        assert_eq!(r.route(up, KeySource::KeyCode, || true, t0), Route::Swallow);
        assert_eq!(r.route(up, KeySource::SystemDefined, || true, t0), Route::Swallow);
        assert_eq!(r.route(up, KeySource::SystemDefined, || true, t0), Route::Pass, "only once");
        // Repeats keep acting.
        let rep = MediaKeyEvent { repeat: true, ..down };
        assert_eq!(r.route(rep, KeySource::SystemDefined, || true, t0 + Duration::from_millis(400)), Route::Act);
        assert_eq!(r.route(rep, KeySource::SystemDefined, || true, t0 + Duration::from_millis(480)), Route::Act);
        // A later keycode press (after the twin window) acts on its own.
        assert_eq!(r.route(down, KeySource::KeyCode, || true, t0 + Duration::from_secs(2)), Route::Act);
    }
}
