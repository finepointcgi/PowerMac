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

// ---------------------------------------------------------------------------
// Smooth scrolling
// ---------------------------------------------------------------------------

use crate::smooth::{SmoothParams, Smoother, ACCEL_WINDOW_NS};

fn params(step: f64, accel: f64) -> SmoothParams {
    SmoothParams { step, duration_ns: 200 * MS, acceleration: accel, easing: Easing::EaseOutCubic }
}

/// Run the smoother at 8 ms frames until idle; returns per-frame outputs.
fn run(s: &mut Smoother, from: u64, p: &SmoothParams) -> Vec<[i64; 2]> {
    let mut out = Vec::new();
    let mut t = from;
    while s.is_active() {
        t += 8 * MS;
        out.push(s.tick(t, p));
        assert!(out.len() < 10_000, "smoother never finished");
    }
    out
}

#[test]
fn old_json_still_loads_and_smoothing_defaults_off() {
    let old = r#"{"enabled":true,"reverseMouseVertical":false,"reverseMouseHorizontal":false,
        "reverseTrackpadVertical":false,"reverseTrackpadHorizontal":false,"windowsTouchpadHeuristic":false}"#;
    let c: Config = serde_json::from_str(old).unwrap();
    assert!(!c.smooth_scrolling && c.smooth_horizontal);
    assert_eq!((c.smooth_step_px, c.smooth_duration_ms, c.smooth_acceleration), (60, 250, 100));
    assert_eq!(c.smooth_easing, Easing::EaseOutCubic);
    let json = serde_json::to_value(Config { smooth_easing: Easing::EaseInOutCubic, ..c }).unwrap();
    assert_eq!(json["smoothScrolling"], false);
    assert_eq!(json["smoothStepPx"], 60);
    assert_eq!(json["smoothEasing"], "easeInOutCubic");
}

#[test]
fn shared_config_roundtrips_smooth_fields() {
    let c = Config {
        smooth_scrolling: true,
        smooth_horizontal: false,
        smooth_step_px: 123,
        smooth_duration_ms: 400,
        smooth_acceleration: 250,
        smooth_easing: Easing::Linear,
        ..Config::default()
    };
    let s = SharedConfig::new(c);
    assert_eq!(s.load(), c);
    // Out-of-range values are clamped.
    s.store(Config { smooth_step_px: 0, smooth_duration_ms: 99_999, smooth_acceleration: 9_999, ..c });
    let l = s.load();
    assert_eq!(l.smooth_step_px, *SMOOTH_STEP_RANGE.start());
    assert_eq!(l.smooth_duration_ms, *SMOOTH_DURATION_RANGE.end());
    assert_eq!(l.smooth_acceleration, SMOOTH_ACCEL_MAX);
}

#[test]
fn easing_curves_are_monotonic_and_end_at_one() {
    for e in Easing::ALL {
        assert_eq!(e.apply(0.0), 0.0, "{e:?}");
        assert!((e.apply(1.0) - 1.0).abs() < 1e-12, "{e:?}");
        let mut prev = 0.0;
        for i in 0..=100 {
            let v = e.apply(i as f64 / 100.0);
            assert!(v >= prev - 1e-12, "{e:?} not monotonic");
            prev = v;
        }
        assert_eq!(Easing::from_u8(e.to_u8()), e);
    }
    // Ease-out is front-loaded.
    assert!(Easing::EaseOutCubic.apply(0.25) > 0.5);
}

#[test]
fn one_notch_glides_its_full_step_over_the_duration() {
    let p = params(60.0, 0.0);
    let mut s = Smoother::new();
    assert_eq!(s.push(0, 1.0, 0, &p), 60.0);
    let out = run(&mut s, 0, &p);
    let sum: i64 = out.iter().map(|o| o[0]).sum();
    assert_eq!(sum, 60);
    assert!(out.iter().all(|o| o[1] == 0));
    // Spread over the duration (200 ms at 8 ms frames = 25 frames), front-loaded.
    assert!((24..=27).contains(&out.len()), "{} frames", out.len());
    assert!(out[0][0] > out[out.len() - 2][0]);
    assert!(out.iter().all(|o| o[0] >= 0));
}

#[test]
fn negative_and_horizontal_notches() {
    let p = params(50.0, 0.0);
    let mut s = Smoother::new();
    s.push(1, -1.0, 0, &p);
    let out = run(&mut s, 0, &p);
    assert_eq!(out.iter().map(|o| o[1]).sum::<i64>(), -50);
    assert!(out.iter().all(|o| o[0] == 0 && o[1] <= 0));
}

#[test]
fn slow_notches_add_up_without_acceleration() {
    let p = params(40.0, 1.0);
    let mut s = Smoother::new();
    let mut t = 0;
    let mut sum = 0;
    for _ in 0..5 {
        s.push(0, 1.0, t, &p);
        // Notches 100 ms apart: outside the acceleration window.
        for _ in 0..12 {
            t += 8 * MS + MS / 3;
            sum += s.tick(t, &p)[0];
        }
    }
    sum += run(&mut s, t, &p).iter().map(|o| o[0]).sum::<i64>();
    assert_eq!(sum, 200);
}

#[test]
fn fast_spin_accelerates() {
    let p = params(40.0, 1.0);
    let mut s = Smoother::new();
    let gap = ACCEL_WINDOW_NS / 2;
    let added: Vec<f64> = (0..5).map(|i| s.push(0, 1.0, i * gap, &p)).collect();
    assert_eq!(added[0], 40.0);
    assert_eq!(added[1], 40.0 * Smoother::accel_multiplier(1.0, 1));
    assert!(added.windows(2).all(|w| w[1] > w[0]));
    let total: f64 = added.iter().sum();
    let sum: i64 = run(&mut s, 4 * gap, &p).iter().map(|o| o[0]).sum();
    assert_eq!(sum, total.round() as i64);
    // Acceleration off: constant step.
    let p0 = params(40.0, 0.0);
    let mut s = Smoother::new();
    assert!((0..5).all(|i| s.push(0, 1.0, i * gap, &p0) == 40.0));
}

#[test]
fn direction_change_cancels_the_glide() {
    let p = params(100.0, 0.0);
    let mut s = Smoother::new();
    s.push(0, 1.0, 0, &p);
    let first = s.tick(16 * MS, &p)[0];
    assert!(first > 0);
    s.push(0, -1.0, 20 * MS, &p);
    let rest: i64 = run(&mut s, 20 * MS, &p).iter().map(|o| o[0]).sum();
    // Only the new (downward) notch is emitted from here on.
    assert_eq!(rest, -100);
}

#[test]
fn fractional_windows_notches() {
    let p = Config::default().smooth_params(WIN_UNITS_PER_PX);
    assert_eq!(p.step, 120.0);
    let mut s = Smoother::new();
    s.push(0, 0.25, 0, &p); // a 30-unit high-resolution step
    let sum: i64 = run(&mut s, 0, &p).iter().map(|o| o[0]).sum();
    assert_eq!(sum, 30);
}

#[test]
fn smoothing_applies_only_to_mouse_without_modifiers() {
    let on = Config { smooth_scrolling: true, ..Config::default() };
    assert!(on.smooth_for(Device::Mouse, false, false));
    assert!(on.smooth_for(Device::Mouse, true, false));
    assert!(!on.smooth_for(Device::Trackpad, false, false));
    assert!(!on.smooth_for(Device::Mouse, false, true));
    assert!(!Config { smooth_horizontal: false, ..on }.smooth_for(Device::Mouse, true, false));
    assert!(!Config { enabled: false, ..on }.smooth_for(Device::Mouse, false, false));
    assert!(!Config::default().smooth_for(Device::Mouse, false, false));
}

#[test]
fn windows_plan_reverses_then_smooths() {
    let c = Config { smooth_scrolling: true, ..Config::default() };
    // Reverse vertical (default) then smooth: one notch down.
    assert_eq!(windows_plan(&c, false, 120, 0, false), WinPlan::Smooth(-1.0));
    // Horizontal not reversed, smoothed as is.
    assert_eq!(windows_plan(&c, true, -240, 0, false), WinPlan::Smooth(-2.0));
    // Modifier held: reversal only.
    assert_eq!(windows_plan(&c, false, 120, 0, true), WinPlan::Reinject(-120));
    // Our own and pm-input's injections pass.
    assert_eq!(windows_plan(&c, false, 120, SELF_INJECTED_TAG, false), WinPlan::Pass);
    assert_eq!(windows_plan(&c, false, 120, PM_INPUT_INJECTED_TAG, false), WinPlan::Pass);
    // Touchpad (heuristic) is never smoothed.
    let h = Config { windows_touchpad_heuristic: true, ..c };
    assert_eq!(windows_plan(&h, false, 30, 0, false), WinPlan::Pass);
    // Smoothing off: same as windows_action.
    let d = Config::default();
    assert_eq!(windows_plan(&d, false, 120, 0, false), WinPlan::Reinject(-120));
    assert_eq!(windows_plan(&d, true, 120, 0, false), WinPlan::Pass);
}

#[test]
fn mac_notch_direction_uses_line_then_fixed_delta() {
    assert_eq!(mac_notch_direction(3, 2.7), 1.0);
    assert_eq!(mac_notch_direction(-1, -0.1), -1.0);
    assert_eq!(mac_notch_direction(0, -0.1), -1.0);
    assert_eq!(mac_notch_direction(0, 0.0), 0.0);
}
