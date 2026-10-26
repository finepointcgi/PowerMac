use crate::*;

const MS: u64 = 1_000_000;

fn wheel() -> MacScrollFields {
    MacScrollFields { is_continuous: false, scroll_phase: 0, momentum_phase: 0 }
}
fn gesture(phase: i64) -> MacScrollFields {
    MacScrollFields { is_continuous: true, scroll_phase: phase, momentum_phase: 0 }
}
fn momentum(phase: i64) -> MacScrollFields {
    MacScrollFields { is_continuous: true, scroll_phase: 0, momentum_phase: phase }
}
fn continuous_unphased() -> MacScrollFields {
    MacScrollFields { is_continuous: true, scroll_phase: 0, momentum_phase: 0 }
}

#[test]
fn config_defaults_and_serde() {
    let c = Config::default();
    assert!(c.enabled && c.reverse_mouse_vertical);
    assert!(!c.reverse_mouse_horizontal && !c.reverse_trackpad_vertical);
    assert!(!c.reverse_trackpad_horizontal && !c.windows_touchpad_heuristic);

    let json = serde_json::to_value(c).unwrap();
    assert_eq!(json["reverseMouseVertical"], true);
    assert_eq!(json["reverseTrackpadHorizontal"], false);

    // Missing fields fall back to defaults.
    let c: Config = serde_json::from_str(r#"{"reverseTrackpadVertical":true}"#).unwrap();
    assert!(c.reverse_trackpad_vertical && c.reverse_mouse_vertical && c.enabled);
    let c: Config = serde_json::from_str("{}").unwrap();
    assert_eq!(c, Config::default());
}

#[test]
fn bits_roundtrip_all_combinations() {
    for b in 0..64u32 {
        assert_eq!(Config::from_bits(b).to_bits(), b);
    }
}

#[test]
fn reverse_for_respects_flags_and_master_switch() {
    let c = Config {
        reverse_mouse_vertical: true,
        reverse_mouse_horizontal: false,
        reverse_trackpad_vertical: false,
        reverse_trackpad_horizontal: true,
        ..Config::default()
    };
    assert_eq!(c.reverse_for(Device::Mouse), Reverse { vertical: true, horizontal: false });
    assert_eq!(c.reverse_for(Device::Trackpad), Reverse { vertical: false, horizontal: true });
    let off = Config { enabled: false, ..c };
    assert_eq!(off.reverse_for(Device::Mouse), Reverse::NONE);
    assert_eq!(off.reverse_for(Device::Trackpad), Reverse::NONE);
}

#[test]
fn discrete_wheel_is_mouse() {
    let mut cl = MacClassifier::new();
    assert_eq!(cl.classify(wheel(), 0), Device::Mouse);
    // Even right after a trackpad gesture.
    cl.classify(gesture(scroll_phase::CHANGED), 10 * MS);
    assert_eq!(cl.classify(wheel(), 11 * MS), Device::Mouse);
}

#[test]
fn trackpad_gesture_and_momentum_stay_trackpad() {
    let mut cl = MacClassifier::new();
    let mut t = 0;
    for f in [
        gesture(scroll_phase::MAY_BEGIN),
        gesture(scroll_phase::BEGAN),
        gesture(scroll_phase::CHANGED),
        gesture(scroll_phase::CHANGED),
        gesture(scroll_phase::ENDED),
        momentum(momentum_phase::BEGIN),
        momentum(momentum_phase::CONTINUE),
        momentum(momentum_phase::CONTINUE),
        momentum(momentum_phase::END),
        gesture(scroll_phase::CANCELLED),
    ] {
        t += 16 * MS;
        assert_eq!(cl.classify(f, t), Device::Trackpad, "{f:?}");
    }
    // Long momentum fling (several seconds) is still trackpad: it carries a phase.
    assert_eq!(cl.classify(momentum(momentum_phase::CONTINUE), t + 5_000 * MS), Device::Trackpad);
}

#[test]
fn unphased_continuous_depends_on_recent_gesture() {
    let mut cl = MacClassifier::new();
    // Smooth-scrolling mouse driver with no gesture context: mouse.
    assert_eq!(cl.classify(continuous_unphased(), 1_000 * MS), Device::Mouse);
    // Straggler right after a gesture: trackpad.
    cl.classify(gesture(scroll_phase::ENDED), 2_000 * MS);
    assert_eq!(cl.classify(continuous_unphased(), 2_100 * MS), Device::Trackpad);
    // Long after: mouse again.
    assert_eq!(
        cl.classify(continuous_unphased(), 2_000 * MS + MacClassifier::GESTURE_GRACE_NS + 1),
        Device::Mouse
    );
}

#[test]
fn deltas_reverse_selected_axes_only() {
    let d = MacDeltas { delta: [3, -1], point: [30, -10], fixed: [2.5, -0.75] };
    assert_eq!(d.reversed(Reverse::NONE), d);
    assert_eq!(
        d.reversed(Reverse { vertical: true, horizontal: false }),
        MacDeltas { delta: [-3, -1], point: [-30, -10], fixed: [-2.5, -0.75] }
    );
    assert_eq!(
        d.reversed(Reverse { vertical: false, horizontal: true }),
        MacDeltas { delta: [3, 1], point: [30, 10], fixed: [2.5, 0.75] }
    );
    let both = Reverse { vertical: true, horizontal: true };
    assert_eq!(d.reversed(both).reversed(both), d);
}

#[test]
fn windows_classification_heuristic() {
    assert_eq!(classify_windows_delta(120, false), Device::Mouse);
    assert_eq!(classify_windows_delta(17, false), Device::Mouse);
    assert_eq!(classify_windows_delta(-240, true), Device::Mouse);
    assert_eq!(classify_windows_delta(17, true), Device::Trackpad);
}

#[test]
fn windows_actions() {
    let c = Config::default();
    assert_eq!(windows_action(&c, false, 120, 0), WinAction::Reinject(-120));
    assert_eq!(windows_action(&c, false, -360, 0), WinAction::Reinject(360));
    assert_eq!(windows_action(&c, true, 120, 0), WinAction::Pass);
    // Own and pm-input injections are never touched.
    assert_eq!(windows_action(&c, false, 120, SELF_INJECTED_TAG), WinAction::Pass);
    assert_eq!(windows_action(&c, false, 120, PM_INPUT_INJECTED_TAG), WinAction::Pass);
    assert_eq!(windows_action(&c, false, 0, 0), WinAction::Pass);
    // Disabled.
    let off = Config { enabled: false, ..c };
    assert_eq!(windows_action(&off, false, 120, 0), WinAction::Pass);
    // Heuristic: small deltas count as touchpad, which is not reversed by default.
    let h = Config { windows_touchpad_heuristic: true, ..c };
    assert_eq!(windows_action(&h, false, 30, 0), WinAction::Pass);
    assert_eq!(windows_action(&h, false, 120, 0), WinAction::Reinject(-120));
    let hz = Config { reverse_mouse_horizontal: true, ..c };
    assert_eq!(windows_action(&hz, true, -120, 0), WinAction::Reinject(120));
}

#[test]
fn permission_error_is_detectable() {
    let e: anyhow::Error = Error::PermissionDenied("x".into()).into();
    assert!(is_permission_error(&e));
    assert!(!is_permission_error(&anyhow::anyhow!("other")));
}

#[cfg(target_os = "macos")]
#[test]
fn natural_scrolling_query_does_not_panic() {
    let _ = system_natural_scrolling().unwrap();
}
