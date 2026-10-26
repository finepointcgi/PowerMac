//! Applying [`PointerSettings`] to one macOS HID service while remembering
//! the original values, independent of the HID API (so it is unit-tested with
//! a fake property store on every platform).
//!
//! The HID pointer properties are effectively write-only: a set goes to the
//! event system's pointer filter, while a read returns the driver's registry
//! value (resolution) or nothing (acceleration; the system-wide value is
//! readable from the client). So the state here never trusts a read-back:
//! it remembers the original once and what it last set, and the caller
//! decides when to force a re-set (new device, the system value changed,
//! periodic re-assert).

use std::collections::HashMap;

use crate::{mac_targets, PointerSettings};

pub(crate) const KEY_RESOLUTION: &str = "HIDPointerResolution";

/// Integer properties of one device.
pub(crate) trait PropStore {
    /// The device's value, or the system-wide value when the device does not
    /// report one.
    fn get(&self, key: &str) -> Option<i64>;
    fn set(&self, key: &str, value: i64) -> bool;
}

/// What we changed on one device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Touched {
    /// Value before we first changed the key (restored later).
    pub orig: HashMap<String, i64>,
    /// Value we last set.
    pub last: HashMap<String, i64>,
}

impl Touched {
    pub(crate) fn is_empty(&self) -> bool {
        self.orig.is_empty()
    }

    /// The system value of `key` changed to `value` (e.g. the user moved the
    /// System Settings slider): that is the new original, and ours must be
    /// set again. Returns true when this device was affected.
    pub(crate) fn system_changed(&mut self, key: &str, value: i64) -> bool {
        match self.orig.get_mut(key) {
            Some(o) => {
                *o = value;
                self.last.remove(key);
                true
            }
            None => false,
        }
    }
}

/// Bring the device in line with `s`. `accel_key` is the device's
/// acceleration property. `force` re-sets values even if unchanged.
/// Returns true when something was written.
pub(crate) fn apply_device(
    p: &dyn PropStore,
    accel_key: &str,
    s: &PointerSettings,
    t: &mut Touched,
    force: bool,
) -> bool {
    let mut wrote = false;
    let orig_res = t.orig.get(KEY_RESOLUTION).copied().or_else(|| p.get(KEY_RESOLUTION));
    let targets = mac_targets(s, orig_res);
    for (key, target) in [(accel_key, targets.acceleration), (KEY_RESOLUTION, targets.resolution)] {
        match target {
            Some(v) => {
                if !t.orig.contains_key(key) {
                    match p.get(key) {
                        Some(c) => {
                            t.orig.insert(key.to_string(), c);
                        }
                        // Unknown original: we could never restore it, so leave it alone.
                        None => continue,
                    }
                }
                if force || t.last.get(key) != Some(&v) {
                    if !p.set(key, v) {
                        log::warn!("mouse-tuning: could not set {key}");
                    }
                    t.last.insert(key.to_string(), v);
                    wrote = true;
                }
            }
            None => {
                if let Some(o) = t.orig.remove(key) {
                    p.set(key, o);
                    t.last.remove(key);
                    wrote = true;
                }
            }
        }
    }
    wrote
}

/// Put back every original value.
pub(crate) fn restore_device(p: &dyn PropStore, t: &mut Touched) {
    for (k, o) in t.orig.drain() {
        if !p.set(&k, o) {
            log::warn!("mouse-tuning: could not restore {k}");
        }
    }
    t.last.clear();
}
