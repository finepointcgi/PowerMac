//! Hover / leave state machine (pure; time is passed in).
//!
//! * Resting on an app icon for `delay` shows its panel.
//! * While a panel is open, moving to another app icon switches after the
//!   shorter `switch_delay`.
//! * The panel stays open while the pointer is on its icon, on the panel or
//!   on the bridge between them; after leaving all three it closes once
//!   `leave_grace` has passed without coming back.
//! * A dismissal (Esc, a click, the Dock hiding) closes the panel and keeps
//!   it closed until the pointer leaves that icon.

use std::time::{Duration, Instant};

/// What the pointer is over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target<K> {
    /// A running app's Dock icon.
    Icon(K),
    /// The preview panel.
    Panel,
    /// Between the icon and the panel.
    Bridge,
    /// Anything else.
    Outside,
}

/// What the runtime should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect<K> {
    /// Show (or switch) the panel for this app.
    Show(K),
    /// Close the panel.
    Hide,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State<K> {
    Idle,
    Pending { key: K, since: Instant },
    Shown { key: K, next: Option<(K, Instant)>, left: Option<Instant> },
    Suppressed { key: K },
}

/// Timings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub delay: Duration,
    pub switch_delay: Duration,
    pub leave_grace: Duration,
}

impl Timing {
    /// Defaults for a given hover delay.
    pub fn new(delay: Duration) -> Self {
        Timing { delay, switch_delay: delay.min(Duration::from_millis(120)), leave_grace: Duration::from_millis(220) }
    }
}

/// The state machine. `K` identifies an app (e.g. its bundle path).
#[derive(Debug, Clone)]
pub struct Hover<K> {
    timing: Timing,
    state: State<K>,
}

impl<K: Clone + PartialEq> Hover<K> {
    pub fn new(timing: Timing) -> Self {
        Hover { timing, state: State::Idle }
    }

    pub fn set_timing(&mut self, t: Timing) {
        self.timing = t;
    }

    /// App whose panel is (logically) open.
    pub fn shown(&self) -> Option<&K> {
        match &self.state {
            State::Shown { key, .. } => Some(key),
            _ => None,
        }
    }

    /// App waiting for the hover delay.
    pub fn pending(&self) -> Option<&K> {
        match &self.state {
            State::Pending { key, .. } => Some(key),
            State::Shown { next: Some((k, _)), .. } => Some(k),
            _ => None,
        }
    }

    /// When [`Hover::tick`] next has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        match &self.state {
            State::Pending { since, .. } => Some(*since + self.timing.delay),
            State::Shown { next, left, .. } => {
                let a = next.as_ref().map(|(_, t)| *t + self.timing.switch_delay);
                let b = left.map(|t| t + self.timing.leave_grace);
                match (a, b) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                }
            }
            _ => None,
        }
    }

    /// The pointer is over `t` at `now`.
    pub fn pointer(&mut self, now: Instant, t: Target<K>) -> Vec<Effect<K>> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match (state, t) {
            (State::Idle, Target::Icon(k)) => State::Pending { key: k, since: now },
            (State::Idle, _) => State::Idle,

            (State::Pending { key, since }, Target::Icon(k)) => {
                if k == key {
                    State::Pending { key, since }
                } else {
                    State::Pending { key: k, since: now }
                }
            }
            (State::Pending { .. }, _) => State::Idle,

            (State::Suppressed { key }, Target::Icon(k)) => {
                if k == key {
                    State::Suppressed { key }
                } else {
                    State::Pending { key: k, since: now }
                }
            }
            (State::Suppressed { .. }, _) => State::Idle,

            (State::Shown { key, next, .. }, Target::Icon(k)) => {
                if k == key {
                    State::Shown { key, next: None, left: None }
                } else {
                    let next = match next {
                        Some((n, t)) if n == k => Some((n, t)),
                        _ => Some((k, now)),
                    };
                    State::Shown { key, next, left: None }
                }
            }
            (State::Shown { key, .. }, Target::Panel | Target::Bridge) => State::Shown { key, next: None, left: None },
            (State::Shown { key, left, .. }, Target::Outside) => State::Shown { key, next: None, left: Some(left.unwrap_or(now)) },
        };
        self.tick(now)
    }

    /// Advance timers.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect<K>> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        let (s, fx) = match state {
            State::Pending { key, since } if now >= since + self.timing.delay => {
                (State::Shown { key: key.clone(), next: None, left: None }, vec![Effect::Show(key)])
            }
            State::Shown { left: Some(l), .. } if now >= l + self.timing.leave_grace => (State::Idle, vec![Effect::Hide]),
            State::Shown { next: Some((k, t)), .. } if now >= t + self.timing.switch_delay => {
                (State::Shown { key: k.clone(), next: None, left: None }, vec![Effect::Show(k)])
            }
            s => (s, vec![]),
        };
        self.state = s;
        fx
    }

    /// Close because of Esc / a click / the Dock hiding. `on_icon` is the app
    /// icon under the pointer, which won't reopen until the pointer leaves it.
    pub fn dismiss(&mut self, on_icon: Option<K>) -> Vec<Effect<K>> {
        let was_shown = matches!(self.state, State::Shown { .. });
        self.state = match on_icon {
            Some(key) => State::Suppressed { key },
            None => State::Idle,
        };
        if was_shown {
            vec![Effect::Hide]
        } else {
            vec![]
        }
    }

    /// Forget everything (e.g. disabled); hides if shown.
    pub fn reset(&mut self) -> Vec<Effect<K>> {
        self.dismiss(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Clock(Instant);
    impl Clock {
        fn at(&self, ms: u64) -> Instant {
            self.0 + Duration::from_millis(ms)
        }
    }

    fn machine() -> (Hover<&'static str>, Clock) {
        (Hover::new(Timing::new(Duration::from_millis(300))), Clock(Instant::now()))
    }

    use Effect::*;
    use Target::*;

    #[test]
    fn delay_then_show() {
        let (mut h, c) = machine();
        assert!(h.pointer(c.at(0), Icon("a")).is_empty());
        assert_eq!(h.pending(), Some(&"a"));
        assert_eq!(h.deadline(), Some(c.at(300)));
        assert!(h.pointer(c.at(100), Icon("a")).is_empty());
        assert!(h.tick(c.at(299)).is_empty());
        assert_eq!(h.tick(c.at(300)), vec![Show("a")]);
        assert_eq!(h.shown(), Some(&"a"));
        assert!(h.tick(c.at(1000)).is_empty());
    }

    #[test]
    fn moving_between_icons_restarts_the_delay() {
        let (mut h, c) = machine();
        h.pointer(c.at(0), Icon("a"));
        h.pointer(c.at(200), Icon("b"));
        assert!(h.tick(c.at(400)).is_empty());
        assert_eq!(h.tick(c.at(500)), vec![Show("b")]);
    }

    #[test]
    fn leaving_before_the_delay_cancels() {
        let (mut h, c) = machine();
        h.pointer(c.at(0), Icon("a"));
        h.pointer(c.at(100), Outside);
        assert!(h.tick(c.at(1000)).is_empty());
        assert_eq!(h.shown(), None);
        assert_eq!(h.deadline(), None);
    }

    #[test]
    fn zero_delay_shows_immediately() {
        let mut h = Hover::new(Timing::new(Duration::ZERO));
        let t = Instant::now();
        assert_eq!(h.pointer(t, Icon(1)), vec![Show(1)]);
    }

    #[test]
    fn panel_and_bridge_keep_it_open() {
        let (mut h, c) = machine();
        h.pointer(c.at(0), Icon("a"));
        h.tick(c.at(300));
        assert!(h.pointer(c.at(400), Bridge).is_empty());
        assert!(h.pointer(c.at(450), Panel).is_empty());
        assert!(h.tick(c.at(5000)).is_empty());
        assert_eq!(h.shown(), Some(&"a"));
    }

    #[test]
    fn leave_grace_then_hide_and_coming_back_cancels() {
        let (mut h, c) = machine();
        h.pointer(c.at(0), Icon("a"));
        h.tick(c.at(300));
        assert!(h.pointer(c.at(1000), Outside).is_empty());
        assert_eq!(h.deadline(), Some(c.at(1220)));
        // Still outside later: the leave time isn't reset.
        assert!(h.pointer(c.at(1100), Outside).is_empty());
        // Back onto the panel in time.
        assert!(h.pointer(c.at(1200), Panel).is_empty());
        assert!(h.tick(c.at(2000)).is_empty());
        // Leave for good.
        h.pointer(c.at(3000), Outside);
        assert!(h.tick(c.at(3219)).is_empty());
        assert_eq!(h.tick(c.at(3220)), vec![Hide]);
        assert_eq!(h.shown(), None);
        // Re-entering the icon needs the full delay again.
        h.pointer(c.at(4000), Icon("a"));
        assert!(h.tick(c.at(4200)).is_empty());
        assert_eq!(h.tick(c.at(4300)), vec![Show("a")]);
    }

    #[test]
    fn switching_while_shown_uses_switch_delay() {
        let (mut h, c) = machine();
        h.pointer(c.at(0), Icon("a"));
        h.tick(c.at(300));
        assert!(h.pointer(c.at(500), Icon("b")).is_empty());
        assert_eq!(h.shown(), Some(&"a"));
        assert_eq!(h.pending(), Some(&"b"));
        assert!(h.tick(c.at(619)).is_empty());
        assert_eq!(h.tick(c.at(620)), vec![Show("b")]);
        assert_eq!(h.shown(), Some(&"b"));
        // Brushing over another icon on the way to the panel doesn't switch.
        h.pointer(c.at(700), Icon("c"));
        h.pointer(c.at(760), Bridge);
        assert!(h.tick(c.at(1000)).is_empty());
        assert_eq!(h.shown(), Some(&"b"));
        // A non-app tile (Outside) while shown starts the leave grace.
        h.pointer(c.at(1100), Outside);
        assert_eq!(h.tick(c.at(1320)), vec![Hide]);
    }

    #[test]
    fn dismiss_suppresses_until_leaving_the_icon() {
        let (mut h, c) = machine();
        h.pointer(c.at(0), Icon("a"));
        h.tick(c.at(300));
        assert_eq!(h.dismiss(Some("a")), vec![Hide]);
        assert!(h.pointer(c.at(400), Icon("a")).is_empty());
        assert!(h.tick(c.at(2000)).is_empty());
        assert_eq!(h.shown(), None);
        // Another icon right away: normal delay.
        h.pointer(c.at(2100), Icon("b"));
        assert_eq!(h.tick(c.at(2400)), vec![Show("b")]);
        // Dismissing when nothing is shown emits nothing.
        h.dismiss(None);
        assert!(h.dismiss(None).is_empty());
        // After leaving, the same icon works again.
        h.pointer(c.at(3000), Icon("b"));
        assert_eq!(h.tick(c.at(3300)), vec![Show("b")]);
        assert_eq!(h.reset(), vec![Hide]);
    }

    #[test]
    fn suppressed_then_outside_then_back() {
        let (mut h, c) = machine();
        h.pointer(c.at(0), Icon("a"));
        h.dismiss(Some("a")); // e.g. clicked the icon before the delay
        h.pointer(c.at(100), Icon("a"));
        assert!(h.tick(c.at(1000)).is_empty());
        h.pointer(c.at(1100), Outside);
        h.pointer(c.at(1200), Icon("a"));
        assert_eq!(h.tick(c.at(1500)), vec![Show("a")]);
    }
}
