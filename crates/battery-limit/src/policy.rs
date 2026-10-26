//! The pure charge/inhibit/discharge state machine.
//!
//! [`step`] takes the config, the previous [`PolicyState`] and a fresh
//! [`PolicyInput`] and returns what the helper should do. No I/O, so every
//! rule is unit-tested below.

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// The state the helper should put the SMC in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    /// Firmware default: adapter on, charging allowed.
    Charge,
    /// Adapter on (the Mac runs from it), charging inhibited.
    Inhibit,
    /// Adapter off: the battery powers the Mac and discharges.
    Discharge,
}

/// Hysteresis kept between steps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyState {
    /// Reached the limit; don't charge again until below `limit - sailing`.
    pub sailing: bool,
    /// Heat protection tripped; clears 2 °C below the threshold.
    pub hot: bool,
    /// One-shot "charge to 100 % now", cleared when the adapter is unplugged.
    pub top_up: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PolicyInput {
    pub percent: Option<u8>,
    pub plugged: bool,
    pub temperature_c: Option<f32>,
    /// The helper is installed and answering.
    pub helper_ready: bool,
    /// The system is about to sleep.
    pub sleeping: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// `None`: send nothing (helper missing).
    pub action: Option<Action>,
    pub reason: String,
    pub needs_helper: bool,
}

const HEAT_HYSTERESIS_C: f32 = 2.0;

impl Decision {
    fn act(action: Action, reason: impl Into<String>) -> Decision {
        Decision {
            action: Some(action),
            reason: reason.into(),
            needs_helper: false,
        }
    }
}

/// What to do at system sleep given the current action. The adapter is
/// always turned back on (never discharge unattended); charging is
/// re-enabled unless the user chose to keep the limit during sleep.
pub fn sleep_action(keep_limit_during_sleep: bool, current: Action) -> Action {
    match (keep_limit_during_sleep, current) {
        (false, _) => Action::Charge,
        (true, Action::Discharge) => Action::Inhibit,
        (true, a) => a,
    }
}

pub fn step(cfg: &Config, st: &mut PolicyState, inp: &PolicyInput) -> Decision {
    let cfg = cfg.normalized();
    if st.top_up && !inp.plugged {
        st.top_up = false;
    }

    if !cfg.enabled {
        st.sailing = false;
        st.hot = false;
        return if inp.helper_ready {
            Decision::act(Action::Charge, "limit off")
        } else {
            Decision {
                action: None,
                reason: "limit off".into(),
                needs_helper: false,
            }
        };
    }
    if !inp.helper_ready {
        return Decision {
            action: None,
            reason: "helper not installed".into(),
            needs_helper: true,
        };
    }
    let Some(p) = inp.percent else {
        return Decision::act(Action::Charge, "no battery");
    };

    // Heat protection (with hysteresis).
    let heat = cfg.heat_protection_celsius as f32;
    st.hot = match inp.temperature_c {
        Some(t) if heat > 0.0 && t >= heat => true,
        Some(t) if heat > 0.0 && st.hot && t > heat - HEAT_HYSTERESIS_C => true,
        _ => false,
    };

    // Sailing hysteresis, tracked whether plugged in or not, so replugging
    // in the sailing band doesn't immediately start a charge.
    let limit = cfg.limit_percent;
    if p >= limit {
        st.sailing = true;
    } else if (p as i16) < limit as i16 - cfg.sailing_percent as i16 {
        st.sailing = false;
    }

    let decision = if !inp.plugged {
        Decision::act(Action::Charge, "on battery")
    } else if st.top_up {
        if st.hot {
            Decision::act(
                Action::Inhibit,
                format!("top-up paused: battery too hot (≥ {heat:.0} °C)"),
            )
        } else {
            Decision::act(Action::Charge, "charging to 100 % until unplugged")
        }
    } else if limit >= 100 {
        if st.hot {
            Decision::act(
                Action::Inhibit,
                format!("paused: battery too hot (≥ {heat:.0} °C)"),
            )
        } else {
            Decision::act(Action::Charge, "limit 100 %")
        }
    } else if cfg.discharge_to_limit && p > limit {
        Decision::act(Action::Discharge, format!("discharging to {limit} %"))
    } else if st.hot {
        Decision::act(
            Action::Inhibit,
            format!("paused: battery too hot (≥ {heat:.0} °C)"),
        )
    } else if st.sailing {
        if p >= limit {
            Decision::act(Action::Inhibit, format!("holding at {limit} %"))
        } else {
            Decision::act(
                Action::Inhibit,
                format!(
                    "sailing until {} %",
                    limit.saturating_sub(cfg.sailing_percent)
                ),
            )
        }
    } else {
        Decision::act(Action::Charge, format!("charging to {limit} %"))
    };

    if inp.sleeping {
        let a = sleep_action(
            cfg.keep_limit_during_sleep,
            decision.action.unwrap_or(Action::Charge),
        );
        return Decision::act(
            a,
            format!(
                "sleep: {}",
                if a == Action::Charge {
                    "charging allowed"
                } else {
                    "limit kept"
                }
            ),
        );
    }
    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Config {
        Config {
            enabled: true,
            ..Default::default()
        }
    }

    fn inp(p: u8, plugged: bool) -> PolicyInput {
        PolicyInput {
            percent: Some(p),
            plugged,
            temperature_c: Some(25.0),
            helper_ready: true,
            sleeping: false,
        }
    }

    fn act(cfg: &Config, st: &mut PolicyState, i: PolicyInput) -> Action {
        step(cfg, st, &i).action.unwrap()
    }

    #[test]
    fn disabled_restores_charging_or_does_nothing() {
        let cfg = Config::default();
        let mut st = PolicyState {
            sailing: true,
            hot: true,
            top_up: false,
        };
        assert_eq!(act(&cfg, &mut st, inp(90, true)), Action::Charge);
        assert!(!st.sailing && !st.hot);
        let d = step(
            &cfg,
            &mut st,
            &PolicyInput {
                helper_ready: false,
                ..inp(90, true)
            },
        );
        assert_eq!(d.action, None);
        assert!(!d.needs_helper, "disabled never asks for the helper");
    }

    #[test]
    fn helper_missing_reports_needs_helper_and_sends_nothing() {
        let d = step(
            &on(),
            &mut PolicyState::default(),
            &PolicyInput {
                helper_ready: false,
                ..inp(95, true)
            },
        );
        assert_eq!(d.action, None);
        assert!(d.needs_helper);
    }

    #[test]
    fn charges_to_limit_then_holds() {
        let cfg = on();
        let mut st = PolicyState::default();
        assert_eq!(act(&cfg, &mut st, inp(50, true)), Action::Charge);
        assert_eq!(act(&cfg, &mut st, inp(79, true)), Action::Charge);
        assert_eq!(act(&cfg, &mut st, inp(80, true)), Action::Inhibit);
        assert_eq!(
            act(&cfg, &mut st, inp(85, true)),
            Action::Inhibit,
            "above limit, no discharge"
        );
    }

    #[test]
    fn sailing_band() {
        let cfg = on(); // limit 80, sail 5
        let mut st = PolicyState::default();
        assert_eq!(act(&cfg, &mut st, inp(80, true)), Action::Inhibit);
        for p in [79, 77, 75] {
            assert_eq!(
                act(&cfg, &mut st, inp(p, true)),
                Action::Inhibit,
                "{p}% is in the sailing band"
            );
        }
        assert_eq!(act(&cfg, &mut st, inp(74, true)), Action::Charge);
        assert_eq!(
            act(&cfg, &mut st, inp(78, true)),
            Action::Charge,
            "keeps charging up to the limit"
        );
        assert_eq!(act(&cfg, &mut st, inp(80, true)), Action::Inhibit);
    }

    #[test]
    fn sailing_zero_pins_at_limit() {
        let cfg = Config {
            sailing_percent: 0,
            ..on()
        };
        let mut st = PolicyState::default();
        assert_eq!(act(&cfg, &mut st, inp(80, true)), Action::Inhibit);
        assert_eq!(act(&cfg, &mut st, inp(79, true)), Action::Charge);
    }

    #[test]
    fn unplug_allows_charge_and_replug_respects_sailing() {
        let cfg = on();
        let mut st = PolicyState::default();
        assert_eq!(act(&cfg, &mut st, inp(80, true)), Action::Inhibit);
        // Unplugged: nothing to hold; leave the SMC in the default state.
        assert_eq!(act(&cfg, &mut st, inp(78, false)), Action::Charge);
        // Replugged inside the sailing band: still sailing.
        assert_eq!(act(&cfg, &mut st, inp(77, true)), Action::Inhibit);
        // Replugged well below: charge.
        assert_eq!(act(&cfg, &mut st, inp(60, false)), Action::Charge);
        assert_eq!(act(&cfg, &mut st, inp(60, true)), Action::Charge);
    }

    #[test]
    fn discharge_to_limit() {
        let cfg = Config {
            discharge_to_limit: true,
            ..on()
        };
        let mut st = PolicyState::default();
        assert_eq!(act(&cfg, &mut st, inp(95, true)), Action::Discharge);
        assert_eq!(act(&cfg, &mut st, inp(81, true)), Action::Discharge);
        assert_eq!(act(&cfg, &mut st, inp(80, true)), Action::Inhibit);
        assert_eq!(act(&cfg, &mut st, inp(78, true)), Action::Inhibit);
        // Never discharge on battery (nothing to turn off).
        assert_eq!(act(&cfg, &mut st, inp(95, false)), Action::Charge);
    }

    #[test]
    fn heat_protection_with_hysteresis() {
        let cfg = on();
        let mut st = PolicyState::default();
        let hot = |t: f32| PolicyInput {
            temperature_c: Some(t),
            ..inp(50, true)
        };
        assert_eq!(act(&cfg, &mut st, hot(34.9)), Action::Charge);
        assert_eq!(act(&cfg, &mut st, hot(35.0)), Action::Inhibit);
        assert_eq!(
            act(&cfg, &mut st, hot(33.5)),
            Action::Inhibit,
            "within hysteresis"
        );
        assert_eq!(act(&cfg, &mut st, hot(33.0)), Action::Charge);
        // Off when 0.
        let cfg0 = Config {
            heat_protection_celsius: 0,
            ..on()
        };
        assert_eq!(
            act(&cfg0, &mut PolicyState::default(), hot(50.0)),
            Action::Charge
        );
        // Unknown temperature doesn't block charging.
        let unknown = PolicyInput {
            temperature_c: None,
            ..inp(50, true)
        };
        assert_eq!(act(&cfg, &mut st, unknown), Action::Charge);
    }

    #[test]
    fn heat_does_not_override_discharge() {
        let cfg = Config {
            discharge_to_limit: true,
            ..on()
        };
        let i = PolicyInput {
            temperature_c: Some(40.0),
            ..inp(90, true)
        };
        assert_eq!(act(&cfg, &mut PolicyState::default(), i), Action::Discharge);
    }

    #[test]
    fn top_up_until_unplug() {
        let cfg = Config {
            discharge_to_limit: true,
            ..on()
        };
        let mut st = PolicyState {
            top_up: true,
            ..Default::default()
        };
        assert_eq!(act(&cfg, &mut st, inp(85, true)), Action::Charge);
        assert_eq!(act(&cfg, &mut st, inp(100, true)), Action::Charge);
        assert!(st.top_up);
        // Heat still pauses a top-up.
        let hot = PolicyInput {
            temperature_c: Some(40.0),
            ..inp(90, true)
        };
        assert_eq!(act(&cfg, &mut st, hot), Action::Inhibit);
        // Unplug ends it.
        assert_eq!(act(&cfg, &mut st, inp(99, false)), Action::Charge);
        assert!(!st.top_up);
        assert_eq!(act(&cfg, &mut st, inp(99, true)), Action::Discharge);
    }

    #[test]
    fn sleep_safety() {
        assert_eq!(sleep_action(false, Action::Inhibit), Action::Charge);
        assert_eq!(sleep_action(false, Action::Discharge), Action::Charge);
        assert_eq!(sleep_action(true, Action::Inhibit), Action::Inhibit);
        assert_eq!(sleep_action(true, Action::Discharge), Action::Inhibit);
        assert_eq!(sleep_action(true, Action::Charge), Action::Charge);

        let sleeping = PolicyInput {
            sleeping: true,
            ..inp(90, true)
        };
        assert_eq!(
            act(&on(), &mut PolicyState::default(), sleeping.clone()),
            Action::Charge
        );
        let keep = Config {
            keep_limit_during_sleep: true,
            discharge_to_limit: true,
            ..on()
        };
        assert_eq!(
            act(&keep, &mut PolicyState::default(), sleeping),
            Action::Inhibit
        );
    }

    #[test]
    fn no_battery_and_limit_100() {
        let i = PolicyInput {
            percent: None,
            ..inp(0, true)
        };
        assert_eq!(act(&on(), &mut PolicyState::default(), i), Action::Charge);
        let cfg = Config {
            limit_percent: 100,
            ..on()
        };
        assert_eq!(
            act(&cfg, &mut PolicyState::default(), inp(100, true)),
            Action::Charge
        );
    }
}
