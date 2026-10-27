//! Dispatch semantics, against the fake backend.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pm_hook::testing::{Delivered, FakeEvent, FakeHub};
use pm_hook::{Emit, EventMask, EventType, Guard, Status, Subscription, TapState, Verdict};

type Log = Arc<Mutex<Vec<String>>>;

fn log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

/// A subscriber that records "name:code" and returns `verdict(code)`.
fn rec(hub: &FakeHub, log: &Log, name: &'static str, prio: i32, verdict: impl Fn(&FakeEvent) -> Verdict + Send + 'static) -> Guard {
    let log = log.clone();
    hub.subscribe(Subscription::new(name, prio, EventMask::ALL, move |ev| {
        let f = ev.fake().unwrap();
        log.lock().unwrap().push(format!("{name}:{}{}", f.code, if f.note.is_empty() { String::new() } else { format!("/{}", f.note) }));
        verdict(f)
    }))
}

fn names(log: &Log) -> Vec<String> {
    std::mem::take(&mut *log.lock().unwrap())
}

fn orig(code: u32) -> Delivered {
    Delivered::Original(FakeEvent::key_down(code))
}

#[test]
fn priority_order_and_ties() {
    let hub = FakeHub::new();
    let l = log();
    let _a = rec(&hub, &l, "low", 10, |_| Verdict::Pass);
    let _b = rec(&hub, &l, "high", 100, |_| Verdict::Pass);
    let _c = rec(&hub, &l, "mid1", 50, |_| Verdict::Pass);
    let _d = rec(&hub, &l, "mid2", 50, |_| Verdict::Pass);
    assert_eq!(hub.send(FakeEvent::key_down(1)), vec![orig(1)]);
    assert_eq!(names(&l), ["high:1", "mid1:1", "mid2:1", "low:1"]);
    let order: Vec<String> = hub.hub().subscribers().into_iter().map(|(n, _)| n).collect();
    assert_eq!(order, ["high", "mid1", "mid2", "low"]);
}

#[test]
fn swallow_stops_propagation() {
    let hub = FakeHub::new();
    let l = log();
    let _a = rec(&hub, &l, "a", 3, |_| Verdict::Pass);
    let _b = rec(&hub, &l, "b", 2, |f| if f.code == 7 { Verdict::Swallow } else { Verdict::Pass });
    let _c = rec(&hub, &l, "c", 1, |_| Verdict::Pass);
    assert!(hub.send(FakeEvent::key_down(7)).is_empty());
    assert_eq!(names(&l), ["a:7", "b:7"]);
    assert_eq!(hub.send(FakeEvent::key_down(8)), vec![orig(8)]);
    assert_eq!(names(&l), ["a:8", "b:8", "c:8"]);
}

#[test]
fn modified_in_place_is_seen_downstream_and_delivered() {
    let hub = FakeHub::new();
    let l = log();
    let _a = hub.subscribe(Subscription::new("remap", 2, EventMask::KEYS, |ev| {
        let f = ev.fake().unwrap();
        if f.code == 1 {
            f.code = 2;
            f.note = "remapped".into();
            return Verdict::Modified;
        }
        Verdict::Pass
    }));
    let _b = rec(&hub, &l, "after", 1, |_| Verdict::Pass);
    let out = hub.send(FakeEvent::key_down(1));
    let mut want = FakeEvent::key_down(2);
    want.note = "remapped".into();
    assert_eq!(out, vec![Delivered::Original(want)]);
    assert_eq!(names(&l), ["after:2/remapped"]);
}

#[test]
fn replace_dispatches_synthetics_to_lower_subscribers_only() {
    let hub = FakeHub::new();
    let l = log();
    let _top = rec(&hub, &l, "top", 30, |_| Verdict::Pass);
    let _km = rec(&hub, &l, "km", 20, |f| {
        if f.code == 1 && f.tag == 0 {
            Verdict::Replace(vec![FakeEvent::key_down(10).tagged(0x4B4D).emit(), FakeEvent::key_down(11).tagged(0x4B4D).emit()])
        } else {
            Verdict::Pass
        }
    });
    let _low = rec(&hub, &l, "low", 10, |_| Verdict::Pass);
    let out = hub.send(FakeEvent::key_down(1));
    assert_eq!(
        out,
        vec![
            Delivered::Synthetic(FakeEvent::key_down(10).tagged(0x4B4D)),
            Delivered::Synthetic(FakeEvent::key_down(11).tagged(0x4B4D))
        ]
    );
    // "top" saw only the original; "low" only the replacements, in order.
    assert_eq!(names(&l), ["top:1", "km:1", "low:10", "low:11"]);
}

#[test]
fn replace_with_original_posts_first_then_passes() {
    let hub = FakeHub::new();
    let l = log();
    let _hyper = rec(&hub, &l, "hyper", 2, |f| {
        if f.code == 5 {
            Verdict::Replace(vec![FakeEvent::key_down(100).emit(), Emit::Original])
        } else {
            Verdict::Pass
        }
    });
    let _low = rec(&hub, &l, "low", 1, |_| Verdict::Pass);
    let out = hub.send(FakeEvent::key_down(5));
    assert_eq!(out, vec![Delivered::Synthetic(FakeEvent::key_down(100)), orig(5)]);
    assert_eq!(names(&l), ["hyper:5", "low:100", "low:5"]);
}

#[test]
fn lower_subscribers_can_swallow_or_replace_synthetics() {
    let hub = FakeHub::new();
    let l = log();
    let _a = rec(&hub, &l, "a", 3, |f| {
        if f.code == 1 {
            Verdict::Replace(vec![FakeEvent::key_down(2).emit(), FakeEvent::key_down(3).emit()])
        } else {
            Verdict::Pass
        }
    });
    let _b = rec(&hub, &l, "b", 2, |f| match f.code {
        2 => Verdict::Swallow,
        3 => Verdict::Replace(vec![Emit::Original, FakeEvent::key_down(4).emit()]),
        _ => Verdict::Pass,
    });
    let _c = rec(&hub, &l, "c", 1, |_| Verdict::Pass);
    let out = hub.send(FakeEvent::key_down(1));
    assert_eq!(out, vec![Delivered::Synthetic(FakeEvent::key_down(3)), Delivered::Synthetic(FakeEvent::key_down(4))]);
    assert_eq!(names(&l), ["a:1", "b:2", "b:3", "c:3", "c:4"]);
}

#[test]
fn without_recursion_synthetics_are_only_collected() {
    // Windows: SendInput'ed replacements come back through the hook.
    let hub = FakeHub::without_recursion();
    let l = log();
    let _a = rec(&hub, &l, "a", 2, |f| if f.code == 1 { Verdict::Replace(vec![FakeEvent::key_down(9).emit()]) } else { Verdict::Pass });
    let _b = rec(&hub, &l, "b", 1, |_| Verdict::Pass);
    assert_eq!(hub.send(FakeEvent::key_down(1)), vec![Delivered::Synthetic(FakeEvent::key_down(9))]);
    assert_eq!(names(&l), ["a:1"]);
    // ... but an emitted Original still continues down the chain.
    let _c = rec(&hub, &l, "c", 3, |f| if f.code == 2 { Verdict::Replace(vec![FakeEvent::key_down(8).emit(), Emit::Original]) } else { Verdict::Pass });
    assert_eq!(hub.send(FakeEvent::key_down(2)), vec![Delivered::Synthetic(FakeEvent::key_down(8)), orig(2)]);
    assert_eq!(names(&l), ["c:2", "a:2", "b:2"]);
}

#[test]
fn masks_filter_event_types() {
    let hub = FakeHub::new();
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let _g = hub.subscribe(Subscription::new("scroll", 1, EventMask::SCROLL, move |_| {
        n2.fetch_add(1, Ordering::SeqCst);
        Verdict::Swallow
    }));
    assert_eq!(hub.send(FakeEvent::key_down(1)), vec![orig(1)]);
    assert!(hub.send(FakeEvent::new(EventType::ScrollWheel, 120)).is_empty());
    assert_eq!(n.load(Ordering::SeqCst), 1);
    assert!(EventMask::KEYS.contains(EventType::FlagsChanged));
    assert!(!EventMask::KEYS.has_mouse());
    assert!(EventMask::MOVES.contains(EventType::LeftMouseDragged));
    assert!((EventMask::SCROLL | EventType::KeyDown).has_keyboard());
}

#[test]
fn ignore_tags_and_source_tag() {
    let hub = FakeHub::new();
    let l = log();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let _a = hub.subscribe(
        Subscription::new("skips-mwb", 2, EventMask::ALL, move |ev| {
            s2.lock().unwrap().push(ev.source_tag());
            Verdict::Pass
        })
        .ignore_tags(&[pm_hook::tags::MWB, pm_hook::tags::TEXT_EXPANDER]),
    );
    let _b = rec(&hub, &l, "all", 1, |_| Verdict::Pass);
    hub.send(FakeEvent::key_down(1).tagged(pm_hook::tags::MWB));
    hub.send(FakeEvent::key_down(2).tagged(pm_hook::tags::KEYBOARD_MANAGER));
    hub.send(FakeEvent::key_down(3));
    assert_eq!(*seen.lock().unwrap(), vec![pm_hook::tags::KEYBOARD_MANAGER, 0]);
    assert_eq!(names(&l), ["all:1", "all:2", "all:3"]);
    assert_eq!(pm_hook::tags::name(pm_hook::tags::SCROLL_REVERSER), Some("SRVS"));
    assert_eq!(pm_hook::tags::name(0), None);
}

#[test]
fn subscribe_during_dispatch_takes_effect_next_event() {
    let hub = Arc::new(FakeHub::new());
    let l = log();
    let late: Arc<Mutex<Option<Guard>>> = Arc::new(Mutex::new(None));
    let (h2, l2, late2) = (hub.clone(), l.clone(), late.clone());
    let _a = hub.subscribe(Subscription::new("adder", 10, EventMask::ALL, move |_| {
        let mut g = late2.lock().unwrap();
        if g.is_none() {
            let l3 = l2.clone();
            *g = Some(h2.subscribe(Subscription::new("late", 1, EventMask::ALL, move |ev| {
                l3.lock().unwrap().push(format!("late:{}", ev.fake().unwrap().code));
                Verdict::Pass
            })));
        }
        Verdict::Pass
    }));
    hub.send(FakeEvent::key_down(1));
    assert!(names(&l).is_empty(), "a subscriber added during dispatch misses the current event");
    hub.send(FakeEvent::key_down(2));
    assert_eq!(names(&l), ["late:2"]);
    late.lock().unwrap().take();
}

#[test]
fn unsubscribe_during_dispatch() {
    let hub = FakeHub::new();
    let l = log();
    let victim: Arc<Mutex<Option<Guard>>> = Arc::new(Mutex::new(None));
    let selfg: Arc<Mutex<Option<Guard>>> = Arc::new(Mutex::new(None));
    let v2 = victim.clone();
    let _killer = rec(&hub, &l, "killer", 10, move |f| {
        if f.code == 1 {
            v2.lock().unwrap().take(); // drops the lower subscriber mid-dispatch
        }
        Verdict::Pass
    });
    *victim.lock().unwrap() = Some(rec(&hub, &l, "victim", 5, |_| Verdict::Pass));
    let (s2, l2) = (selfg.clone(), l.clone());
    *selfg.lock().unwrap() = Some(hub.subscribe(Subscription::new("once", 1, EventMask::ALL, move |ev| {
        l2.lock().unwrap().push(format!("once:{}", ev.fake().unwrap().code));
        s2.lock().unwrap().take(); // unsubscribe itself from inside its handler
        Verdict::Pass
    })));
    hub.send(FakeEvent::key_down(0));
    assert_eq!(names(&l), ["killer:0", "victim:0", "once:0"]);
    hub.send(FakeEvent::key_down(1));
    assert_eq!(names(&l), ["killer:1"], "victim removed mid-dispatch, once removed itself");
    assert_eq!(hub.hub().subscribers().len(), 1);
}

#[test]
fn unsubscribe_from_another_thread_waits_for_running_handler() {
    let hub = Arc::new(FakeHub::new());
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let done = Arc::new(AtomicUsize::new(0));
    let d2 = done.clone();
    let g = hub.subscribe(Subscription::new("slow", 1, EventMask::ALL, move |_| {
        let _ = entered_tx.send(());
        std::thread::sleep(Duration::from_millis(150));
        d2.store(1, Ordering::SeqCst);
        Verdict::Pass
    }));
    let h2 = hub.clone();
    let t = std::thread::spawn(move || h2.send(FakeEvent::key_down(1)));
    entered_rx.recv().unwrap();
    drop(g); // must block until the handler returned
    assert_eq!(done.load(Ordering::SeqCst), 1);
    t.join().unwrap();
    // Never called again.
    assert_eq!(hub.send(FakeEvent::key_down(2)), vec![orig(2)]);
}

#[test]
fn panicking_handler_passes() {
    let hub = FakeHub::new();
    let l = log();
    let _a = hub.subscribe(Subscription::new("boom", 2, EventMask::ALL, |_| panic!("handler bug")));
    let _b = rec(&hub, &l, "after", 1, |_| Verdict::Pass);
    assert_eq!(hub.send(FakeEvent::key_down(1)), vec![orig(1)]);
    assert_eq!(names(&l), ["after:1"]);
}

#[test]
fn health_propagates_to_subscribers() {
    let hub = FakeHub::new();
    let seen: Arc<Mutex<Vec<Status>>> = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let g = hub.subscribe(Subscription::new("ws", 1, EventMask::KEYS, |_| Verdict::Pass).on_status(move |st| s2.lock().unwrap().push(st.clone())));
    // Initial call with the current status.
    assert_eq!(seen.lock().unwrap().last().unwrap().state, TapState::Active);
    hub.set_state(TapState::Disabled);
    hub.reenabled();
    hub.set_secure_input(true);
    let states: Vec<(TapState, bool, u64)> = seen.lock().unwrap().iter().map(|s| (s.state.clone(), s.secure_input, s.reenabled)).collect();
    assert_eq!(
        &states[states.len() - 3..],
        &[(TapState::Disabled, false, 0), (TapState::Active, false, 1), (TapState::Active, true, 1)]
    );
    // No duplicate notification without a change.
    let n = seen.lock().unwrap().len();
    hub.set_secure_input(true);
    assert_eq!(seen.lock().unwrap().len(), n);
    drop(g);
    assert_eq!(hub.status().state, TapState::Idle);
}

#[test]
fn creation_failure_is_reported() {
    let hub = FakeHub::new();
    hub.fail_creation(Some("no Accessibility"));
    let seen: Arc<Mutex<Vec<TapState>>> = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let _g = hub.subscribe(Subscription::new("km", 1, EventMask::KEYS, |_| Verdict::Pass).on_status(move |st| s2.lock().unwrap().push(st.state.clone())));
    assert_eq!(hub.status().state, TapState::Unavailable("no Accessibility".into()));
    assert!(seen.lock().unwrap().contains(&TapState::Unavailable("no Accessibility".into())));
    // Permission granted: the next reconcile (retry) brings it up and tells everyone.
    hub.fail_creation(None);
    let _h = hub.subscribe(Subscription::new("other", 1, EventMask::SCROLL, |_| Verdict::Pass));
    assert_eq!(hub.status().state, TapState::Active);
    assert_eq!(seen.lock().unwrap().last(), Some(&TapState::Active));
}

#[test]
fn backend_mask_is_the_union() {
    let hub = FakeHub::new();
    let a = hub.subscribe(Subscription::new("a", 1, EventMask::KEYS, |_| Verdict::Pass));
    let b = hub.subscribe(Subscription::new("b", 1, EventMask::SCROLL, |_| Verdict::Pass));
    drop(a);
    drop(b);
    assert_eq!(hub.reconciles(), vec![EventMask::KEYS, EventMask::KEYS | EventMask::SCROLL, EventMask::SCROLL, EventMask::NONE]);
}

#[test]
fn stats_count_calls_and_slow_handlers() {
    let hub = FakeHub::new();
    let _g = hub.subscribe(Subscription::new("slowpoke", 1, EventMask::ALL, |ev| {
        if ev.fake().unwrap().code == 1 {
            std::thread::sleep(pm_hook::SLOW_HANDLER + Duration::from_millis(2));
        }
        Verdict::Pass
    }));
    hub.send(FakeEvent::key_down(1));
    hub.send(FakeEvent::key_down(2));
    let s = &hub.hub().stats()[0];
    assert_eq!((s.name.as_str(), s.calls, s.slow), ("slowpoke", 2, 1));
    assert!(s.max >= pm_hook::SLOW_HANDLER);
}

#[test]
fn worker_runs_jobs_in_order_and_drains_on_drop() {
    let w = pm_hook::Worker::new("test-worker").unwrap();
    let v = Arc::new(Mutex::new(Vec::new()));
    for i in 0..5 {
        let v = v.clone();
        assert!(w.submit(move || {
            std::thread::sleep(Duration::from_millis(5));
            v.lock().unwrap().push(i);
        }));
    }
    drop(w);
    assert_eq!(*v.lock().unwrap(), vec![0, 1, 2, 3, 4]);
    let (tx, rx) = std::sync::mpsc::channel();
    pm_hook::defer(move || tx.send(42).unwrap());
    assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 42);
}

#[test]
fn listen_only_runs_only_subscribers_that_allow_it() {
    let hub = FakeHub::new();
    let l = log();
    let _a = rec(&hub, &l, "active-only", 2, |_| Verdict::Swallow);
    let l2 = l.clone();
    let _b = hub.subscribe(
        Subscription::new("observer", 1, EventMask::ALL, move |ev| {
            l2.lock().unwrap().push(format!("observer:{}", ev.fake().unwrap().code));
            Verdict::Swallow
        })
        .allow_listen_only(),
    );
    hub.send_listen_only(FakeEvent::key_down(4));
    assert_eq!(names(&l), ["observer:4"]);
}
