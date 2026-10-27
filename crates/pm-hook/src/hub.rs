//! Subscriber registry, ordered dispatch and status fan-out. Platform
//! independent; the OS side is a [`Backend`].

use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use crate::{Emit, Event, EventMask, Handler, Status, StatusFn, Subscription, Synthetic, TapState, Verdict, SLOW_HANDLER};

/// The OS side of a [`Hub`].
pub(crate) trait Backend: Send + Sync {
    /// The subscriber set changed (`mask` = union of the masks; empty when
    /// there are no subscribers). Make the tap match; `wait` = block until
    /// it does (false on the dispatch thread).
    fn reconcile(&self, hub: &Arc<Hub>, wait: bool);
    /// Live health check (may re-enable a silently disabled tap).
    fn is_healthy(&self, hub: &Hub) -> bool {
        hub.status().is_active()
    }
    /// macOS: dispatch emitted synthetic events to lower subscribers.
    fn recurse_synthetics(&self) -> bool;
    /// Diagnostics: disable the tap behind the watchdog's back.
    fn debug_disable(&self) -> bool {
        false
    }
}

/// Backend for unsupported platforms: never has a tap.
#[allow(dead_code)]
pub(crate) struct NoBackend;

impl Backend for NoBackend {
    fn reconcile(&self, hub: &Arc<Hub>, _wait: bool) {
        let state = if hub.union_mask().is_empty() {
            TapState::Idle
        } else {
            TapState::Unavailable("global input hooks are not supported on this platform".into())
        };
        hub.update_status(|s| s.state = state);
    }
    fn recurse_synthetics(&self) -> bool {
        false
    }
}

thread_local! {
    static ON_DISPATCH_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Mark the current thread as the one that dispatches (handlers run here).
pub(crate) fn mark_dispatch_thread(on: bool) {
    ON_DISPATCH_THREAD.with(|c| c.set(on));
}

pub(crate) fn on_dispatch_thread() -> bool {
    ON_DISPATCH_THREAD.with(|c| c.get())
}

pub(crate) struct Sub {
    id: u64,
    name: String,
    priority: i32,
    mask: EventMask,
    ignore_tags: Vec<usize>,
    handler: Mutex<Handler>,
    on_status: Option<StatusFn>,
    listen_only: bool,
    active: AtomicBool,
    calls: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
    slow: AtomicU64,
    last_warn: Mutex<Option<Instant>>,
}

/// Timing of one subscriber's handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriberStats {
    pub name: String,
    pub priority: i32,
    pub calls: u64,
    pub total: Duration,
    pub max: Duration,
    /// Calls slower than [`SLOW_HANDLER`].
    pub slow: u64,
}

#[derive(Default)]
struct Registry {
    next_id: u64,
    /// Dispatch order: priority descending, then id ascending.
    subs: Arc<[Arc<Sub>]>,
}

/// A subscriber registry with ordered dispatch. [`crate::global`] is the
/// process-wide one; [`crate::testing::FakeHub`] drives one without an OS tap.
pub struct Hub {
    me: Weak<Hub>,
    reg: Mutex<Registry>,
    status: Mutex<Status>,
    backend: Box<dyn Backend>,
}

/// Unsubscribes when dropped.
#[must_use = "dropping the guard unsubscribes"]
pub struct Guard {
    hub: Weak<Hub>,
    id: u64,
}

impl Guard {
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pm_hook::Guard({})", self.id)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(h) = self.hub.upgrade() {
            h.unsubscribe(self.id);
        }
    }
}

/// Result of dispatching one event: what to deliver, in order.
#[derive(Debug)]
pub(crate) enum Out {
    /// The event that was dispatched (possibly edited in place).
    Original,
    Synth(Synthetic),
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Hub {
    pub(crate) fn with_backend(backend: Box<dyn Backend>) -> Arc<Hub> {
        Arc::new_cyclic(|me| Hub {
            me: me.clone(),
            reg: Mutex::new(Registry::default()),
            status: Mutex::new(Status::default()),
            backend,
        })
    }

    fn arc(&self) -> Arc<Hub> {
        self.me.upgrade().expect("hub alive")
    }

    /// Add a subscriber (see [`crate::subscribe`]).
    pub fn subscribe(&self, s: Subscription) -> Guard {
        let sub = {
            let mut reg = lock(&self.reg);
            reg.next_id += 1;
            let sub = Arc::new(Sub {
                id: reg.next_id,
                name: s.name,
                priority: s.priority,
                mask: s.mask,
                ignore_tags: s.ignore_tags,
                handler: Mutex::new(s.handler),
                on_status: s.on_status,
                listen_only: s.listen_only,
                active: AtomicBool::new(true),
                calls: AtomicU64::new(0),
                total_ns: AtomicU64::new(0),
                max_ns: AtomicU64::new(0),
                slow: AtomicU64::new(0),
                last_warn: Mutex::new(None),
            });
            let mut v: Vec<Arc<Sub>> = reg.subs.iter().cloned().collect();
            v.push(sub.clone());
            v.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
            reg.subs = v.into();
            sub
        };
        log::debug!("pm-hook: + {} (priority {})", sub.name, sub.priority);
        let hub = self.arc();
        self.backend.reconcile(&hub, !on_dispatch_thread());
        if let Some(f) = &sub.on_status {
            let st = self.status();
            f(&st);
        }
        Guard { hub: Arc::downgrade(&hub), id: sub.id }
    }

    fn unsubscribe(&self, id: u64) {
        let removed = {
            let mut reg = lock(&self.reg);
            let Some(sub) = reg.subs.iter().find(|s| s.id == id).cloned() else { return };
            reg.subs = reg.subs.iter().filter(|s| s.id != id).cloned().collect::<Vec<_>>().into();
            sub
        };
        removed.active.store(false, Ordering::SeqCst);
        if !on_dispatch_thread() {
            // Wait for a call in progress (the handler mutex is held during
            // the call); afterwards dispatch sees `active == false`.
            drop(lock(&removed.handler));
        }
        log::debug!("pm-hook: - {}", removed.name);
        let hub = self.arc();
        self.backend.reconcile(&hub, !on_dispatch_thread());
    }

    /// Union of all subscribers' masks.
    pub(crate) fn union_mask(&self) -> EventMask {
        lock(&self.reg).subs.iter().fold(EventMask::NONE, |m, s| m | s.mask)
    }

    pub(crate) fn snapshot(&self) -> Arc<[Arc<Sub>]> {
        lock(&self.reg).subs.clone()
    }

    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    pub fn is_healthy(&self) -> bool {
        self.backend.is_healthy(self)
    }

    #[doc(hidden)]
    pub fn debug_disable_tap(&self) -> bool {
        self.backend.debug_disable()
    }

    /// Change the status; subscribers are told when it actually changed.
    pub(crate) fn update_status(&self, f: impl FnOnce(&mut Status)) {
        let (changed, st) = {
            let mut s = lock(&self.status);
            let before = s.clone();
            f(&mut s);
            (*s != before, s.clone())
        };
        if changed {
            if st.state != TapState::Active {
                log::info!("pm-hook: {:?}", st);
            } else {
                log::debug!("pm-hook: {:?}", st);
            }
            for sub in self.snapshot().iter() {
                if let Some(cb) = &sub.on_status {
                    if sub.active.load(Ordering::SeqCst) {
                        cb(&st);
                    }
                }
            }
        }
    }

    pub fn stats(&self) -> Vec<SubscriberStats> {
        self.snapshot()
            .iter()
            .map(|s| SubscriberStats {
                name: s.name.clone(),
                priority: s.priority,
                calls: s.calls.load(Ordering::Relaxed),
                total: Duration::from_nanos(s.total_ns.load(Ordering::Relaxed)),
                max: Duration::from_nanos(s.max_ns.load(Ordering::Relaxed)),
                slow: s.slow.load(Ordering::Relaxed),
            })
            .collect()
    }

    pub fn subscribers(&self) -> Vec<(String, i32)> {
        self.snapshot().iter().map(|s| (s.name.clone(), s.priority)).collect()
    }

    /// Dispatch one event through every subscriber.
    pub(crate) fn dispatch(&self, ev: &mut Event<'_>) -> Vec<Out> {
        let subs = self.snapshot();
        run(&subs, 0, ev, self.backend.recurse_synthetics())
    }

    /// Dispatch from a listen-only tap: only subscribers that allow it are
    /// called, and the result is ignored.
    pub(crate) fn dispatch_listen_only(&self, ev: &mut Event<'_>) {
        let subs: Vec<Arc<Sub>> = self.snapshot().iter().filter(|s| s.listen_only).cloned().collect();
        run(&subs, 0, ev, false);
    }
}

fn call(s: &Sub, ev: &mut Event<'_>) -> Option<Verdict> {
    let mut h = lock(&s.handler);
    // Unsubscribed while we waited for the lock.
    if !s.active.load(Ordering::SeqCst) {
        return None;
    }
    let t0 = Instant::now();
    let v = catch_unwind(AssertUnwindSafe(|| (h)(ev))).unwrap_or_else(|_| {
        log::error!("pm-hook: handler of {} panicked; passing the event", s.name);
        Verdict::Pass
    });
    drop(h);
    let ns = t0.elapsed().as_nanos().min(u64::MAX as u128) as u64;
    s.calls.fetch_add(1, Ordering::Relaxed);
    s.total_ns.fetch_add(ns, Ordering::Relaxed);
    s.max_ns.fetch_max(ns, Ordering::Relaxed);
    if ns >= SLOW_HANDLER.as_nanos() as u64 {
        s.slow.fetch_add(1, Ordering::Relaxed);
        let mut last = lock(&s.last_warn);
        if last.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
            *last = Some(Instant::now());
            log::warn!(
                "pm-hook: {} took {:.1} ms for {:?} (handlers run on the tap thread; move slow work to a pm_hook::Worker)",
                s.name,
                ns as f64 / 1e6,
                ev.kind()
            );
        }
    }
    Some(v)
}

/// Run `ev` through `subs[from..]`. `Out::Original` refers to `ev`.
fn run(subs: &[Arc<Sub>], from: usize, ev: &mut Event<'_>, recurse: bool) -> Vec<Out> {
    for i in from..subs.len() {
        let s = &subs[i];
        if !s.active.load(Ordering::Relaxed) || !s.mask.contains(ev.kind()) || s.ignore_tags.contains(&ev.source_tag()) {
            continue;
        }
        let Some(v) = call(s, ev) else { continue };
        match v {
            Verdict::Pass | Verdict::Modified => {}
            Verdict::Swallow => return Vec::new(),
            Verdict::Replace(emits) => {
                let mut out = Vec::new();
                let mut original_done = false;
                for e in emits {
                    match e {
                        Emit::Original => {
                            if std::mem::replace(&mut original_done, true) {
                                log::warn!("pm-hook: {} emitted the original event twice; ignoring the repeat", s.name);
                                continue;
                            }
                            out.extend(run(subs, i + 1, ev, recurse));
                        }
                        Emit::Event(syn) => {
                            if recurse {
                                out.extend(run_synthetic(subs, i + 1, syn));
                            } else {
                                out.push(Out::Synth(syn));
                            }
                        }
                    }
                }
                return out;
            }
        }
    }
    vec![Out::Original]
}

/// Dispatch an emitted synthetic event to `subs[from..]`; its own
/// `Out::Original` becomes `Out::Synth(syn)`.
fn run_synthetic(subs: &[Arc<Sub>], from: usize, mut syn: Synthetic) -> Vec<Out> {
    let res = match syn_event(&mut syn) {
        Some(mut sev) => run(subs, from, &mut sev, true),
        None => vec![Out::Original],
    };
    let mut syn = Some(syn);
    let mut out = Vec::with_capacity(res.len());
    for o in res {
        match o {
            Out::Original => {
                if let Some(s) = syn.take() {
                    out.push(Out::Synth(s));
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn syn_event(s: &mut Synthetic) -> Option<Event<'_>> {
    match s {
        #[cfg(target_os = "macos")]
        Synthetic::Mac(e) => crate::macos::event_view(e.as_ptr(), true),
        #[cfg(target_os = "windows")]
        Synthetic::Input(_) => None,
        Synthetic::Fake(f) => Some(crate::testing::fake_view(f, true)),
    }
}
