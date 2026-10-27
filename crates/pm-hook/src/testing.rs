//! A hub without an OS tap, for tests: feed it [`FakeEvent`]s and see what
//! would have been delivered.
//!
//! ```
//! use pm_hook::testing::{FakeEvent, FakeHub, Delivered};
//! use pm_hook::{EventMask, EventType, Subscription, Verdict};
//! let hub = FakeHub::new();
//! let _g = hub.subscribe(Subscription::new("eat-a", 1, EventMask::KEYS, |ev| {
//!     if ev.fake().unwrap().code == 'a' as u32 { Verdict::Swallow } else { Verdict::Pass }
//! }));
//! assert!(hub.send(FakeEvent::key_down('a' as u32)).is_empty());
//! assert_eq!(hub.send(FakeEvent::key_down('b' as u32)), vec![Delivered::Original(FakeEvent::key_down('b' as u32))]);
//! ```

use std::sync::{Arc, Mutex};

use crate::hub::{mark_dispatch_thread, Backend, Hub, Out};
use crate::{Event, EventMask, EventType, Guard, Raw, Status, Subscription, Synthetic, TapState};

/// A platform-neutral stand-in for a native event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeEvent {
    pub kind: EventType,
    /// Key code / button number / wheel delta — whatever a test needs.
    pub code: u32,
    pub tag: usize,
    /// Free-form marker handlers may edit in place (`Verdict::Modified`).
    pub note: String,
}

impl FakeEvent {
    pub fn new(kind: EventType, code: u32) -> Self {
        FakeEvent { kind, code, tag: 0, note: String::new() }
    }
    pub fn key_down(code: u32) -> Self {
        Self::new(EventType::KeyDown, code)
    }
    pub fn key_up(code: u32) -> Self {
        Self::new(EventType::KeyUp, code)
    }
    pub fn tagged(mut self, tag: usize) -> Self {
        self.tag = tag;
        self
    }
    /// Wrap as a synthetic event for `Verdict::Replace`.
    pub fn emit(self) -> crate::Emit {
        crate::Emit::Event(Synthetic::Fake(self))
    }
}

pub(crate) fn fake_view(e: &mut FakeEvent, synthetic: bool) -> Event<'_> {
    let (kind, tag) = (e.kind, e.tag);
    Event::new(kind, tag, synthetic, Raw::Fake(e))
}

/// What the fake tap delivered for one input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    /// The input event (as edited by `Modified` handlers).
    Original(FakeEvent),
    /// A synthetic event emitted by a handler (as edited by later ones).
    Synthetic(FakeEvent),
}

#[derive(Default)]
struct FakeState {
    reconciles: Vec<EventMask>,
    fail: Option<String>,
}

struct FakeBackend {
    state: Arc<Mutex<FakeState>>,
    recurse: bool,
}

impl Backend for FakeBackend {
    fn reconcile(&self, hub: &Arc<Hub>, _wait: bool) {
        let mask = hub.union_mask();
        let fail = {
            let mut st = self.state.lock().unwrap();
            st.reconciles.push(mask);
            st.fail.clone()
        };
        let state = match (mask.is_empty(), fail) {
            (true, _) => TapState::Idle,
            (false, Some(e)) => TapState::Unavailable(e),
            (false, None) => TapState::Active,
        };
        hub.update_status(|s| s.state = state);
    }
    fn recurse_synthetics(&self) -> bool {
        self.recurse
    }
}

/// A [`Hub`] driven by hand.
pub struct FakeHub {
    hub: Arc<Hub>,
    state: Arc<Mutex<FakeState>>,
}

impl Default for FakeHub {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeHub {
    /// macOS semantics: emitted events are dispatched to lower subscribers.
    pub fn new() -> Self {
        Self::build(true)
    }

    /// Windows semantics: emitted events are only collected (they would come
    /// back through the hook as injected events).
    pub fn without_recursion() -> Self {
        Self::build(false)
    }

    fn build(recurse: bool) -> Self {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let hub = Hub::with_backend(Box::new(FakeBackend { state: state.clone(), recurse }));
        FakeHub { hub, state }
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    pub fn subscribe(&self, s: Subscription) -> Guard {
        self.hub.subscribe(s)
    }

    /// Dispatch one event, as the tap thread would.
    pub fn send(&self, mut e: FakeEvent) -> Vec<Delivered> {
        mark_dispatch_thread(true);
        let out = {
            let mut ev = fake_view(&mut e, false);
            self.hub.dispatch(&mut ev)
        };
        mark_dispatch_thread(false);
        out.into_iter()
            .filter_map(|o| match o {
                Out::Original => Some(Delivered::Original(e.clone())),
                Out::Synth(Synthetic::Fake(f)) => Some(Delivered::Synthetic(f)),
                #[allow(unreachable_patterns)]
                Out::Synth(_) => None,
            })
            .collect()
    }

    /// Dispatch as a listen-only tap would (nothing can be swallowed).
    pub fn send_listen_only(&self, mut e: FakeEvent) {
        mark_dispatch_thread(true);
        {
            let mut ev = fake_view(&mut e, false);
            self.hub.dispatch_listen_only(&mut ev);
        }
        mark_dispatch_thread(false);
    }

    pub fn status(&self) -> Status {
        self.hub.status()
    }

    /// Simulate a tap state change (e.g. disabled by timeout, lost permission).
    pub fn set_state(&self, state: TapState) {
        self.hub.update_status(|s| s.state = state);
    }

    pub fn set_secure_input(&self, on: bool) {
        self.hub.update_status(|s| s.secure_input = on);
    }

    /// Simulate the watchdog re-enabling the tap.
    pub fn reenabled(&self) {
        self.hub.update_status(|s| {
            s.reenabled += 1;
            s.state = TapState::Active;
        });
    }

    /// Make later tap (re)creations fail (`None`: succeed again).
    pub fn fail_creation(&self, reason: Option<&str>) {
        self.state.lock().unwrap().fail = reason.map(str::to_string);
    }

    /// The union masks the backend was asked to apply, in order.
    pub fn reconciles(&self) -> Vec<EventMask> {
        self.state.lock().unwrap().reconciles.clone()
    }
}
