use std::cell::RefCell;
use std::collections::HashMap;

use crate::applier::{apply_device, restore_device, PropStore, Touched, KEY_RESOLUTION};
use crate::*;

fn dev(kind: DeviceKind, vid: u32, pid: u32, name: &str) -> DeviceInfo {
    DeviceInfo { id: 1, name: name.into(), vendor_id: vid, product_id: pid, kind, transport: "USB".into() }
}

#[test]
fn defaults_change_nothing_but_side_buttons_on_mac() {
    let c = Config::default();
    assert!(c.enabled);
    assert!(c.mouse.is_noop() && c.trackpad.is_noop() && c.devices.is_empty());
    assert_eq!(c.buttons.side_buttons_navigation, cfg!(target_os = "macos"));
    assert_eq!(c.needs_button_hook(), cfg!(target_os = "macos"));
}

#[test]
fn serde_camel_case_and_partial_json() {
    let c: Config = serde_json::from_str("{}").unwrap();
    assert_eq!(c, Config::default());
    let c: Config = serde_json::from_str(
        r#"{"mouse":{"disableAcceleration":true,"speed":0.7},
            "devices":[{"vendorId":1133,"productId":49298,"speed":0.2}],
            "buttons":{"sideButtonsNavigation":false,"mappings":[
                {"button":2,"action":{"type":"missionControl"}},
                {"button":5,"action":{"type":"keystroke","keys":"Cmd+Shift+T"}}]}}"#,
    )
    .unwrap();
    assert!(c.mouse.disable_acceleration && c.mouse.speed == Some(0.7));
    assert!(c.trackpad.is_noop());
    assert_eq!(c.devices[0].vendor_id, 1133);
    assert_eq!(c.devices[0].disable_acceleration, None);
    assert_eq!(c.buttons.mappings[1].action, ButtonAction::Keystroke { keys: "Cmd+Shift+T".into() });
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(v["mouse"]["disableAcceleration"], true);
    assert_eq!(v["buttons"]["mappings"][0]["action"]["type"], "missionControl");
    let back: Config = serde_json::from_value(v).unwrap();
    assert_eq!(back, c);
    // Every action round-trips.
    for a in ButtonAction::all() {
        let s = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<ButtonAction>(&s).unwrap(), a, "{s}");
        assert!(!a.label().is_empty());
    }
}

#[test]
fn effective_settings_use_class_then_override() {
    let mut c = Config {
        mouse: PointerSettings { disable_acceleration: true, speed: Some(0.5) },
        trackpad: PointerSettings { disable_acceleration: false, speed: Some(0.3) },
        ..Config::default()
    };
    c.devices.push(DeviceOverride { vendor_id: 0x46d, product_id: 0xc077, speed: Some(0.9), ..Default::default() });
    c.devices.push(DeviceOverride {
        vendor_id: 0,
        product_id: 0,
        name: "Apple Internal Keyboard / Trackpad".into(),
        disable_acceleration: Some(true),
        speed: None,
    });

    let logi = dev(DeviceKind::Mouse, 0x46d, 0xc077, "USB Optical Mouse");
    assert_eq!(c.effective(&logi), PointerSettings { disable_acceleration: true, speed: Some(0.9) });
    let other = dev(DeviceKind::Mouse, 0x46d, 0x1234, "Other");
    assert_eq!(c.effective(&other), c.mouse);
    let pad = dev(DeviceKind::Trackpad, 0, 0, "Apple Internal Keyboard / Trackpad");
    assert_eq!(c.effective(&pad), PointerSettings { disable_acceleration: true, speed: Some(0.3) });
    // Name must match when given.
    let pad2 = dev(DeviceKind::Trackpad, 0, 0, "Magic Trackpad");
    assert_eq!(c.effective(&pad2), c.trackpad);
    assert!(pad.new_override().matches(&pad));
}

#[test]
fn button_resolution() {
    let mut c = Config::default();
    c.buttons.side_buttons_navigation = true;
    let mac = cfg!(target_os = "macos");
    assert_eq!(c.button_action(button::BACK), if mac { ButtonAction::Back } else { ButtonAction::Default });
    assert_eq!(c.button_action(button::FORWARD), if mac { ButtonAction::Forward } else { ButtonAction::Default });
    assert_eq!(c.button_action(button::MIDDLE), ButtonAction::Default);
    // Left/right are never remapped, even if asked.
    c.buttons.mappings.push(ButtonMapping { button: 0, action: ButtonAction::Paste });
    assert_eq!(c.button_action(0), ButtonAction::Default);
    // Explicit mappings win.
    c.buttons.mappings.push(ButtonMapping { button: button::BACK, action: ButtonAction::MissionControl });
    c.buttons.mappings.push(ButtonMapping { button: button::MIDDLE, action: ButtonAction::Disabled });
    assert_eq!(c.button_action(button::BACK), ButtonAction::MissionControl);
    assert_eq!(c.button_action(button::MIDDLE), ButtonAction::Disabled);
    assert!(c.needs_button_hook());
    let t = c.button_table();
    assert_eq!(t.len(), button::MAX as usize + 1);
    assert_eq!(t[button::BACK as usize], ButtonAction::MissionControl);
    // Master switch off.
    c.enabled = false;
    assert!(!c.needs_button_hook());
    // Navigation off, no mappings.
    let off = Config { buttons: ButtonsConfig { side_buttons_navigation: false, mappings: vec![] }, ..Config::default() };
    assert!(!off.needs_button_hook());
}

#[test]
fn action_plans() {
    use crate::actions::{plan, Step};
    use crate::keys::Combo;
    use pm_input::vk::*;
    assert_eq!(plan(&ButtonAction::Default), None);
    assert_eq!(plan(&ButtonAction::Disabled), None);
    assert_eq!(plan(&ButtonAction::Keystroke { keys: "nonsense+key".into() }), None);
    let paste = plan(&ButtonAction::Paste).unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(plan(&ButtonAction::Back), Some(Step::Keys(Combo { modifiers: vec![VK_LWIN], key: VK_OEM_4 })));
        assert_eq!(plan(&ButtonAction::Forward), Some(Step::Keys(Combo { modifiers: vec![VK_LWIN], key: VK_OEM_6 })));
        assert_eq!(paste, Step::Keys(Combo { modifiers: vec![VK_LWIN], key: b'V' as u32 }));
        assert!(matches!(plan(&ButtonAction::MissionControl), Some(Step::Dock { .. })));
    } else {
        assert_eq!(plan(&ButtonAction::Back), Some(Step::Keys(Combo { modifiers: vec![], key: VK_BROWSER_BACK })));
        assert_eq!(paste, Step::Keys(Combo { modifiers: vec![VK_LCONTROL], key: b'V' as u32 }));
    }
}

#[test]
fn speed_mappings() {
    assert!((linear_speed_factor(0.5) - 1.0).abs() < 1e-12);
    assert!((linear_speed_factor(0.0) - 0.25).abs() < 1e-12);
    assert!((linear_speed_factor(1.0) - 4.0).abs() < 1e-12);
    assert!((linear_speed_factor(7.0) - 4.0).abs() < 1e-12);
    assert!((linear_speed_factor(f64::NAN) - 1.0).abs() < 1e-12);
    assert_eq!(windows_speed(0.0), 1);
    assert_eq!(windows_speed(1.0), 20);
    assert_eq!(windows_speed(0.5), 11);
    assert_eq!(windows_speed(0.47), 10);
}

#[test]
fn mac_targets_for_settings() {
    let res = 400 * FIXED_ONE;
    // Nothing requested.
    assert_eq!(mac_targets(&PointerSettings::default(), Some(res)), MacTargets::default());
    // Linear, native speed.
    let lin = PointerSettings { disable_acceleration: true, speed: None };
    assert_eq!(mac_targets(&lin, Some(res)), MacTargets { acceleration: Some(-FIXED_ONE), resolution: None });
    // Linear, 2x faster = half the resolution.
    let fast = PointerSettings { disable_acceleration: true, speed: Some(0.75) };
    assert_eq!(mac_targets(&fast, Some(res)).resolution, Some(200 * FIXED_ONE));
    // Resolution unknown: acceleration only.
    assert_eq!(mac_targets(&fast, None).resolution, None);
    // With acceleration: tracking speed 0..3.
    let acc = PointerSettings { disable_acceleration: false, speed: Some(0.5) };
    assert_eq!(mac_targets(&acc, Some(res)), MacTargets { acceleration: Some(98_304), resolution: None });
}

/// In-memory property store; records writes.
struct Fake {
    props: RefCell<HashMap<String, i64>>,
    writes: RefCell<Vec<(String, i64)>>,
}

impl Fake {
    fn new(kv: &[(&str, i64)]) -> Self {
        Self {
            props: RefCell::new(kv.iter().map(|(k, v)| (k.to_string(), *v)).collect()),
            writes: RefCell::new(Vec::new()),
        }
    }
    fn get_(&self, k: &str) -> i64 {
        self.props.borrow()[k]
    }
    fn poke(&self, k: &str, v: i64) {
        self.props.borrow_mut().insert(k.into(), v);
    }
}

impl PropStore for Fake {
    fn get(&self, key: &str) -> Option<i64> {
        self.props.borrow().get(key).copied()
    }
    fn set(&self, key: &str, value: i64) -> bool {
        self.writes.borrow_mut().push((key.into(), value));
        self.props.borrow_mut().insert(key.into(), value);
        true
    }
}

const ACC: &str = "HIDMouseAcceleration";

#[test]
fn applier_sets_then_restores_originals() {
    let f = Fake::new(&[(ACC, 45_056), (KEY_RESOLUTION, 400 * FIXED_ONE)]);
    let mut t = Touched::default();
    let s = PointerSettings { disable_acceleration: true, speed: Some(0.75) };
    assert!(apply_device(&f, ACC, &s, &mut t, false));
    assert_eq!(f.get_(ACC), -FIXED_ONE);
    assert_eq!(f.get_(KEY_RESOLUTION), 200 * FIXED_ONE);
    // Re-applying is idempotent (no writes) unless forced.
    let n = f.writes.borrow().len();
    assert!(!apply_device(&f, ACC, &s, &mut t, false));
    assert_eq!(f.writes.borrow().len(), n);
    assert!(apply_device(&f, ACC, &s, &mut t, true));
    assert_eq!(f.writes.borrow().len(), n + 2);
    // Speed back to "system": resolution restored, acceleration still off.
    apply_device(&f, ACC, &PointerSettings { disable_acceleration: true, speed: None }, &mut t, false);
    assert_eq!(f.get_(KEY_RESOLUTION), 400 * FIXED_ONE);
    assert_eq!(f.get_(ACC), -FIXED_ONE);
    restore_device(&f, &mut t);
    assert_eq!(f.get_(ACC), 45_056);
    assert!(t.is_empty());
}

#[test]
fn applier_resolution_is_based_on_the_original_not_a_readback() {
    // Reads keep returning the driver value (writes are not visible), as on macOS.
    struct WriteOnly(Fake);
    impl PropStore for WriteOnly {
        fn get(&self, key: &str) -> Option<i64> {
            self.0.get(key)
        }
        fn set(&self, key: &str, value: i64) -> bool {
            self.0.writes.borrow_mut().push((key.into(), value));
            true
        }
    }
    let f = WriteOnly(Fake::new(&[(ACC, 45_056), (KEY_RESOLUTION, 400 * FIXED_ONE)]));
    let mut t = Touched::default();
    let s = PointerSettings { disable_acceleration: true, speed: Some(1.0) };
    apply_device(&f, ACC, &s, &mut t, false);
    apply_device(&f, ACC, &s, &mut t, true);
    let res: Vec<i64> = f.0.writes.borrow().iter().filter(|(k, _)| k == KEY_RESOLUTION).map(|w| w.1).collect();
    assert_eq!(res, vec![100 * FIXED_ONE, 100 * FIXED_ONE]);
}

#[test]
fn applier_noop_settings_restore_everything() {
    let f = Fake::new(&[(ACC, 45_056), (KEY_RESOLUTION, 400 * FIXED_ONE)]);
    let mut t = Touched::default();
    apply_device(&f, ACC, &PointerSettings { disable_acceleration: true, speed: Some(0.0) }, &mut t, false);
    assert_eq!(f.get_(KEY_RESOLUTION), 1600 * FIXED_ONE);
    apply_device(&f, ACC, &PointerSettings::default(), &mut t, false);
    assert_eq!(f.get_(ACC), 45_056);
    assert_eq!(f.get_(KEY_RESOLUTION), 400 * FIXED_ONE);
    assert!(t.is_empty());
}

#[test]
fn applier_adopts_system_changes_as_new_original() {
    let f = Fake::new(&[(ACC, 45_056), (KEY_RESOLUTION, 400 * FIXED_ONE)]);
    let mut t = Touched::default();
    let s = PointerSettings { disable_acceleration: true, speed: None };
    apply_device(&f, ACC, &s, &mut t, false);
    // The user moves the System Settings slider; the system pushes it to the device.
    f.poke(ACC, 131_072);
    assert!(t.system_changed(ACC, 131_072));
    assert!(!t.system_changed("HIDTrackpadAcceleration", 1));
    assert!(apply_device(&f, ACC, &s, &mut t, false));
    assert_eq!(f.get_(ACC), -FIXED_ONE, "re-applied");
    restore_device(&f, &mut t);
    assert_eq!(f.get_(ACC), 131_072, "the newer value is restored");
}

#[test]
fn applier_leaves_unknown_originals_alone() {
    let f = Fake::new(&[(KEY_RESOLUTION, 400 * FIXED_ONE)]);
    let mut t = Touched::default();
    apply_device(&f, ACC, &PointerSettings { disable_acceleration: true, speed: None }, &mut t, false);
    assert!(f.writes.borrow().is_empty());
    assert!(t.is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn hid_classification() {
    use crate::mac_hid::classify;
    assert_eq!(classify(Some("HIDTrackpadAcceleration"), "Apple Internal Keyboard / Trackpad", false, true), Some(DeviceKind::Trackpad));
    assert_eq!(classify(Some("HIDMouseAcceleration"), "Magic Mouse", false, true), Some(DeviceKind::Mouse));
    assert_eq!(classify(None, "Magic Trackpad", false, true), Some(DeviceKind::Trackpad));
    assert_eq!(classify(None, "USB Mouse", false, true), Some(DeviceKind::Mouse));
    assert_eq!(classify(None, "Keyboard", false, false), None);
}

#[cfg(target_os = "macos")]
#[test]
fn list_devices_does_not_panic() {
    let _ = list_devices();
}

#[cfg(target_os = "macos")]
#[test]
fn dock_notification_symbol_resolves() {
    assert!(crate::macos::dock_available());
}
