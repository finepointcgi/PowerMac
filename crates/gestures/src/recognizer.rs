//! The pure gesture recognizer: touch frames (and physical clicks) in,
//! [`Gesture`]s out. Platform-independent and unit-tested with synthetic
//! frame sequences.
//!
//! A *session* lasts from the first finger down to the last finger up. At
//! most one gesture fires per session (a swipe, a pinch or a click; a tap
//! fires when the session ends without anything else), so lifting fingers
//! one by one after a 4-finger swipe never also fires a 3-finger one.
//!
//! - The finger count has to be **stable** for `stableMs` before movement
//!   is measured from an anchor (centroid and spread of the contacts), so
//!   fingers landing one after another don't count as a 3-finger gesture
//!   on the way to 4. A count change or a different set of contacts
//!   re-anchors.
//! - **Swipe** (3–5 fingers): the centroid moved `swipeDistance` (fraction
//!   of the trackpad's width / height) along one axis, at least
//!   `swipeAxisRatio` times as far as along the other, at an average speed
//!   of `swipeMinVelocity` since the anchor, without the spread changing
//!   like a pinch. An anchor older than `swipeWindowMs` slides forward, so a
//!   slow drag (e.g. three-finger drag) never becomes a swipe.
//! - **Pinch** (2–5 fingers): the spread changed by `pinchRatio` (in:
//!   smaller, out: larger) while the centroid moved less than
//!   `pinchMaxTravel`.
//! - **Tap** (3–5 fingers): the session lasted at most `tapMaxMs`, all
//!   fingers landed within `tapLandingMs` of the first one, none moved more
//!   than `tapMaxTravel`, and there was no physical click.
//! - **Click** (3–5 fingers): a physical click ([`Recognizer::click`])
//!   while that many fingers touch.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{Gesture, PinchDir, SwipeDir};

/// One contact in a frame. Coordinates are normalized to 0..1 with the
/// origin at the bottom left (MultitouchSupport's convention: `y` grows
/// upwards, towards the hinge on a laptop).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    /// Stable for the lifetime of the touch.
    pub id: i32,
    pub x: f32,
    pub y: f32,
    /// Actually touching (not hovering / lifting).
    pub touching: bool,
}

/// All contacts of one device at one moment.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Frame {
    /// Seconds (monotonic; only differences matter).
    pub time: f64,
    pub contacts: Vec<Contact>,
}

/// Recognizer tuning (JSON camelCase; every field optional).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Thresholds {
    pub swipe_distance: f32,
    /// Fraction of the trackpad per second.
    pub swipe_min_velocity: f32,
    pub swipe_axis_ratio: f32,
    pub swipe_window_ms: u32,
    pub pinch_ratio: f32,
    pub pinch_max_travel: f32,
    pub stable_ms: u32,
    pub tap_max_ms: u32,
    pub tap_landing_ms: u32,
    pub tap_max_travel: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            swipe_distance: 0.12,
            swipe_min_velocity: 0.4,
            swipe_axis_ratio: 1.6,
            swipe_window_ms: 600,
            pinch_ratio: 0.3,
            pinch_max_travel: 0.08,
            stable_ms: 40,
            tap_max_ms: 300,
            tap_landing_ms: 120,
            tap_max_travel: 0.03,
        }
    }
}

#[derive(Debug, Clone)]
struct Anchor {
    time: f64,
    centroid: (f32, f32),
    spread: f32,
    ids: Vec<i32>,
}

#[derive(Debug, Clone)]
struct Session {
    start: f64,
    max_fingers: u8,
    max_reached_at: f64,
    /// A gesture fired (or a click happened): nothing else this session.
    done: bool,
    clicked: bool,
    moved: bool,
    first_pos: HashMap<i32, (f32, f32)>,
    count: u8,
    count_since: f64,
    anchor: Option<Anchor>,
}

/// Turns frames into gestures. One per device.
#[derive(Debug, Clone, Default)]
pub struct Recognizer {
    t: Thresholds,
    session: Option<Session>,
    last_time: f64,
}

fn centroid(c: &[Contact]) -> (f32, f32) {
    let n = c.len().max(1) as f32;
    (
        c.iter().map(|p| p.x).sum::<f32>() / n,
        c.iter().map(|p| p.y).sum::<f32>() / n,
    )
}

fn spread(c: &[Contact], at: (f32, f32)) -> f32 {
    let n = c.len().max(1) as f32;
    c.iter()
        .map(|p| ((p.x - at.0).powi(2) + (p.y - at.1).powi(2)).sqrt())
        .sum::<f32>()
        / n
}

impl Recognizer {
    pub fn new(t: Thresholds) -> Self {
        Self {
            t,
            session: None,
            last_time: 0.0,
        }
    }

    pub fn set_thresholds(&mut self, t: Thresholds) {
        self.t = t;
    }

    /// Fingers touching in the current session (0 when none).
    pub fn fingers(&self) -> u8 {
        self.session.as_ref().map_or(0, |s| s.count)
    }

    /// True while a session is in progress.
    pub fn active(&self) -> bool {
        self.session.is_some()
    }

    /// Forget the current session (e.g. the device went away).
    pub fn reset(&mut self) {
        self.session = None;
    }

    /// Feed one frame; returns the gesture it completes, if any.
    pub fn frame(&mut self, f: &Frame) -> Option<Gesture> {
        self.last_time = f.time;
        let touching: Vec<Contact> = f.contacts.iter().copied().filter(|c| c.touching).collect();
        let n = touching.len().min(u8::MAX as usize) as u8;
        let t = self.t;

        if n == 0 {
            let s = self.session.take()?;
            return Self::tap_at_end(&t, &s, f.time);
        }
        let s = self.session.get_or_insert_with(|| Session {
            start: f.time,
            max_fingers: 0,
            max_reached_at: f.time,
            done: false,
            clicked: false,
            moved: false,
            first_pos: HashMap::new(),
            count: 0,
            count_since: f.time,
            anchor: None,
        });
        if n > s.max_fingers {
            s.max_fingers = n;
            s.max_reached_at = f.time;
        }
        for c in &touching {
            let p0 = *s.first_pos.entry(c.id).or_insert((c.x, c.y));
            if ((c.x - p0.0).powi(2) + (c.y - p0.1).powi(2)).sqrt() > t.tap_max_travel {
                s.moved = true;
            }
        }
        if n != s.count {
            s.count = n;
            s.count_since = f.time;
            s.anchor = None;
        }
        if s.done || n < 2 {
            return None;
        }
        let mut ids: Vec<i32> = touching.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let c = centroid(&touching);
        let sp = spread(&touching, c);
        let stable = (f.time - s.count_since) * 1000.0 >= t.stable_ms as f64;
        let reanchor = match &s.anchor {
            None => stable,
            Some(a) => a.ids != ids || (f.time - a.time) * 1000.0 > t.swipe_window_ms as f64,
        };
        if reanchor {
            s.anchor = stable.then_some(Anchor {
                time: f.time,
                centroid: c,
                spread: sp,
                ids,
            });
            return None;
        }
        let a = s.anchor.as_ref()?;
        let (dx, dy) = (c.0 - a.centroid.0, c.1 - a.centroid.1);
        let travel = (dx * dx + dy * dy).sqrt();
        let ratio = if a.spread > 1e-4 { sp / a.spread } else { 1.0 };
        let pinchy = (ratio - 1.0).abs() >= t.pinch_ratio;

        if pinchy && travel <= t.pinch_max_travel && (2..=5).contains(&n) {
            s.done = true;
            let dir = if ratio < 1.0 {
                PinchDir::In
            } else {
                PinchDir::Out
            };
            return Some(Gesture::Pinch { fingers: n, dir });
        }
        if (3..=5).contains(&n) && !pinchy {
            let (ax, ay) = (dx.abs(), dy.abs());
            let (major, minor) = if ax >= ay { (ax, ay) } else { (ay, ax) };
            let dt = (f.time - a.time).max(1e-3) as f32;
            if major >= t.swipe_distance
                && major >= minor * t.swipe_axis_ratio
                && major / dt >= t.swipe_min_velocity
            {
                s.done = true;
                let dir = if ax >= ay {
                    if dx > 0.0 {
                        SwipeDir::Right
                    } else {
                        SwipeDir::Left
                    }
                } else if dy > 0.0 {
                    SwipeDir::Up
                } else {
                    SwipeDir::Down
                };
                return Some(Gesture::Swipe { fingers: n, dir });
            }
        }
        None
    }

    fn tap_at_end(t: &Thresholds, s: &Session, end: f64) -> Option<Gesture> {
        let dur_ms = (end - s.start) * 1000.0;
        let landing_ms = (s.max_reached_at - s.start) * 1000.0;
        (!s.done
            && !s.clicked
            && !s.moved
            && (3..=5).contains(&s.max_fingers)
            && dur_ms <= t.tap_max_ms as f64
            && landing_ms <= t.tap_landing_ms as f64)
            .then_some(Gesture::Tap {
                fingers: s.max_fingers,
            })
    }

    /// A physical click (trackpad pressed) happened now. Returns the
    /// N-finger click for 3–5 fingers; any click cancels this session's tap
    /// and further gestures.
    pub fn click(&mut self) -> Option<Gesture> {
        let s = self.session.as_mut()?;
        s.clicked = true;
        if s.done {
            return None;
        }
        s.done = true;
        (3..=5)
            .contains(&s.count)
            .then_some(Gesture::Click { fingers: s.count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds frame sequences at ~100 Hz.
    struct Sim {
        r: Recognizer,
        t: f64,
        out: Vec<Gesture>,
    }

    const DT: f64 = 0.01;

    impl Sim {
        fn new() -> Self {
            Self {
                r: Recognizer::new(Thresholds::default()),
                t: 1.0,
                out: Vec::new(),
            }
        }
        fn frame(&mut self, pts: &[(i32, f32, f32)]) {
            let f = Frame {
                time: self.t,
                contacts: pts
                    .iter()
                    .map(|&(id, x, y)| Contact {
                        id,
                        x,
                        y,
                        touching: true,
                    })
                    .collect(),
            };
            self.t += DT;
            if let Some(g) = self.r.frame(&f) {
                self.out.push(g);
            }
        }
        fn hold(&mut self, pts: &[(i32, f32, f32)], frames: usize) {
            for _ in 0..frames {
                self.frame(pts);
            }
        }
        fn lift(&mut self) {
            self.frame(&[]);
        }
        /// Move `pts` by (dx, dy) total over `frames` frames.
        fn moving(
            &mut self,
            pts: &[(i32, f32, f32)],
            dx: f32,
            dy: f32,
            frames: usize,
        ) -> Vec<(i32, f32, f32)> {
            let mut cur = pts.to_vec();
            for i in 1..=frames {
                let k = i as f32 / frames as f32;
                cur = pts
                    .iter()
                    .map(|&(id, x, y)| (id, x + dx * k, y + dy * k))
                    .collect();
                self.frame(&cur);
            }
            cur
        }
        /// Scale the spread around the centroid by `factor` over `frames`.
        fn pinching(&mut self, pts: &[(i32, f32, f32)], factor: f32, frames: usize) {
            let (cx, cy) = centroid(
                &pts.iter()
                    .map(|&(id, x, y)| Contact {
                        id,
                        x,
                        y,
                        touching: true,
                    })
                    .collect::<Vec<_>>(),
            );
            for i in 1..=frames {
                let s = 1.0 + (factor - 1.0) * i as f32 / frames as f32;
                let cur: Vec<_> = pts
                    .iter()
                    .map(|&(id, x, y)| (id, cx + (x - cx) * s, cy + (y - cy) * s))
                    .collect();
                self.frame(&cur);
            }
        }
    }

    fn fingers(n: usize) -> Vec<(i32, f32, f32)> {
        (0..n)
            .map(|i| {
                (
                    i as i32 + 1,
                    0.3 + 0.1 * i as f32,
                    0.5 + if i % 2 == 0 { 0.0 } else { 0.03 },
                )
            })
            .collect()
    }

    fn swipe(n: usize, dx: f32, dy: f32) -> Vec<Gesture> {
        let mut s = Sim::new();
        let f = fingers(n);
        s.hold(&f, 6);
        let end = s.moving(&f, dx, dy, 15);
        s.hold(&end, 3);
        s.lift();
        s.out
    }

    #[test]
    fn swipes_every_direction_and_finger_count() {
        for n in [3u8, 4, 5] {
            for (dx, dy, dir) in [
                (0.3, 0.0, SwipeDir::Right),
                (-0.3, 0.0, SwipeDir::Left),
                (0.0, 0.3, SwipeDir::Up),
                (0.0, -0.3, SwipeDir::Down),
            ] {
                assert_eq!(
                    swipe(n as usize, dx, dy),
                    vec![Gesture::Swipe { fingers: n, dir }],
                    "{n} {dir:?}"
                );
            }
        }
    }

    #[test]
    fn two_finger_movement_is_not_a_swipe() {
        assert!(swipe(2, 0.3, 0.0).is_empty(), "two fingers = scrolling");
        assert!(swipe(1, 0.3, 0.0).is_empty());
    }

    #[test]
    fn short_slow_or_diagonal_moves_do_not_swipe() {
        assert!(swipe(3, 0.05, 0.0).is_empty(), "too short");
        assert!(swipe(3, 0.2, 0.18).is_empty(), "diagonal");
        // Slow: 0.3 over 3 s (well under the minimum velocity) — e.g. a
        // three-finger drag of a window.
        let mut s = Sim::new();
        let f = fingers(3);
        s.hold(&f, 6);
        s.moving(&f, 0.3, 0.0, 300);
        s.lift();
        assert!(s.out.is_empty(), "{:?}", s.out);
    }

    #[test]
    fn one_gesture_per_session_and_lift_off_does_not_refire() {
        let mut s = Sim::new();
        let f = fingers(4);
        s.hold(&f, 6);
        let end = s.moving(&f, 0.3, 0.0, 15);
        // Fingers lift one by one, the remaining three keep moving.
        let three: Vec<_> = end[..3].to_vec();
        s.hold(&three, 6);
        s.moving(&three, 0.3, 0.0, 15);
        s.lift();
        assert_eq!(
            s.out,
            vec![Gesture::Swipe {
                fingers: 4,
                dir: SwipeDir::Right
            }]
        );
    }

    #[test]
    fn finger_count_must_be_stable() {
        // Three fingers land and move at once while a fourth lands mid-way:
        // the 3-finger anchor never forms (count changes before stableMs),
        // the swipe is measured with four.
        let mut s = Sim::new();
        let f = fingers(4);
        let three = &f[..3];
        s.frame(three);
        s.frame(three); // 10 ms < stableMs
        s.hold(&f, 6);
        s.moving(&f, -0.3, 0.0, 15);
        s.lift();
        assert_eq!(
            s.out,
            vec![Gesture::Swipe {
                fingers: 4,
                dir: SwipeDir::Left
            }]
        );

        // A contact swapped for another one re-anchors (no jump = no swipe).
        let mut s = Sim::new();
        let a = fingers(3);
        s.hold(&a, 6);
        let b: Vec<_> = a
            .iter()
            .map(|&(id, x, y)| if id == 3 { (9, x + 0.3, y) } else { (id, x, y) })
            .collect();
        s.hold(&b, 40); // (long enough not to be a tap)
        s.lift();
        assert!(s.out.is_empty(), "{:?}", s.out);
    }

    #[test]
    fn pinches() {
        for n in [2u8, 3, 4, 5] {
            for (factor, dir) in [(0.4, PinchDir::In), (2.0, PinchDir::Out)] {
                let mut s = Sim::new();
                let f = fingers(n as usize);
                s.hold(&f, 6);
                s.pinching(&f, factor, 15);
                s.lift();
                assert_eq!(
                    s.out,
                    vec![Gesture::Pinch { fingers: n, dir }],
                    "{n} {dir:?}"
                );
            }
        }
        // A small spread change is neither a pinch nor (stationary) a swipe.
        let mut s = Sim::new();
        let f = fingers(4);
        s.hold(&f, 6);
        s.pinching(&f, 1.15, 15);
        s.hold(&f, 40); // long enough to not be a tap either
        s.lift();
        assert!(s.out.is_empty(), "{:?}", s.out);
    }

    #[test]
    fn taps() {
        for n in [3u8, 4, 5] {
            let mut s = Sim::new();
            s.hold(&fingers(n as usize), 8);
            s.lift();
            assert_eq!(s.out, vec![Gesture::Tap { fingers: n }]);
        }
        // Fingers landing one after another quickly still count as a 4-tap.
        let mut s = Sim::new();
        let f = fingers(4);
        s.frame(&f[..1]);
        s.frame(&f[..2]);
        s.frame(&f[..3]);
        s.hold(&f, 6);
        s.frame(&f[..2]); // lifting
        s.lift();
        assert_eq!(s.out, vec![Gesture::Tap { fingers: 4 }]);
        // 1- and 2-finger taps are macOS's (click / secondary click).
        for n in [1, 2] {
            let mut s = Sim::new();
            s.hold(&fingers(n), 8);
            s.lift();
            assert!(s.out.is_empty());
        }
    }

    #[test]
    fn not_taps() {
        // Held too long.
        let mut s = Sim::new();
        s.hold(&fingers(3), 40);
        s.lift();
        assert!(s.out.is_empty());
        // Slow landing (more than tapLandingMs between first and last finger).
        let mut s = Sim::new();
        let f = fingers(3);
        s.hold(&f[..1], 15);
        s.hold(&f, 5);
        s.lift();
        assert!(s.out.is_empty());
        // Moved.
        let mut s = Sim::new();
        let f = fingers(3);
        s.hold(&f, 2);
        s.moving(&f, 0.05, 0.0, 5);
        s.lift();
        assert!(s.out.is_empty());
        // A swipe isn't also a tap.
        assert_eq!(swipe(3, 0.3, 0.0).len(), 1);
    }

    #[test]
    fn clicks() {
        for n in [3u8, 4, 5] {
            let mut s = Sim::new();
            let f = fingers(n as usize);
            s.hold(&f, 6);
            assert_eq!(s.r.fingers(), n);
            assert_eq!(s.r.click(), Some(Gesture::Click { fingers: n }));
            // A second click in the same session, movement and the lift
            // do nothing more (no tap either).
            assert_eq!(s.r.click(), None);
            s.moving(&f, 0.3, 0.0, 15);
            s.lift();
            assert!(s.out.is_empty(), "{:?}", s.out);
        }
        // An ordinary one/two-finger click is not a gesture and cancels nothing
        // worth cancelling.
        let mut s = Sim::new();
        s.hold(&fingers(2), 5);
        assert_eq!(s.r.click(), None);
        s.lift();
        assert!(s.out.is_empty());
        // Without touches (e.g. a mouse click) nothing happens.
        let mut r = Recognizer::default();
        assert_eq!(r.click(), None);
        // A click that came while 3 fingers touch briefly: no tap afterwards.
        let mut s = Sim::new();
        s.hold(&fingers(3), 3);
        let _ = s.r.click();
        s.lift();
        assert!(s.out.is_empty());
    }

    #[test]
    fn hovering_contacts_are_ignored() {
        let mut r = Recognizer::default();
        let f = Frame {
            time: 1.0,
            contacts: (0..4)
                .map(|i| Contact {
                    id: i,
                    x: 0.5,
                    y: 0.5,
                    touching: false,
                })
                .collect(),
        };
        assert_eq!(r.frame(&f), None);
        assert!(!r.active());
    }

    #[test]
    fn thresholds_serde() {
        let t: Thresholds = serde_json::from_str(r#"{"swipeDistance": 0.2}"#).unwrap();
        assert_eq!(t.swipe_distance, 0.2);
        assert_eq!(t.tap_max_ms, Thresholds::default().tap_max_ms);
        let back: Thresholds = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
    }
}
