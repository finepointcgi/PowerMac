//! End-to-end: two Rust engines on localhost with mock backends.

mod common;

use std::net::SocketAddr;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use mwb::discovery::StaticCandidates;
use mwb::mock::{Injected, MockBackend};
use mwb::{Config, Engine, EngineEvent, EngineHandle};
use pm_input::{Disposition, InputEvent, KeyEvent, MouseEvent};

fn cfg(name: &str, port: u16, other: &str, other_port: u16, key: &str) -> Config {
    let mut c = Config {
        key: key.into(),
        machine_name: name.into(),
        matrix: vec!["A".into(), "B".into()],
        port,
        bind_address: "127.0.0.1".into(),
        swap_cmd_ctrl: false,
        reconnect_secs: 1,
        auto_discover: false,
        ..Default::default()
    };
    c.peers.insert(other.into(), format!("127.0.0.1:{other_port}"));
    c
}

fn pair(relative: bool) -> (EngineHandle, Arc<MockBackend>, EngineHandle, Arc<MockBackend>) {
    let (pa, pb) = (free_base_port(), free_base_port());
    let (ma, mb) = (MockBackend::hd(), MockBackend::hd());
    let mut ca = cfg("A", pa, "B", pb, KEY);
    let mut cb = cfg("B", pb, "A", pa, KEY);
    ca.move_mouse_relatively = relative;
    cb.move_mouse_relatively = relative;
    let a = Engine::start_with(ca, ma.clone(), no_scan()).unwrap();
    let b = Engine::start_with(cb, mb.clone(), no_scan()).unwrap();
    // MWB keeps one socket in each direction.
    assert!(
        wait_until(Duration::from_secs(20), || {
            let (sa, sb) = (a.status(), b.status());
            sa.peers.len() == 1 && sa.peers[0].connections == 2 && sb.peers.len() == 1 && sb.peers[0].connections == 2
        }),
        "engines did not connect: {:?} / {:?}",
        a.status().peers,
        b.status().peers
    );
    std::thread::sleep(Duration::from_millis(150));
    (a, ma, b, mb)
}

fn moves(m: &MockBackend) -> Vec<(i32, i32)> {
    m.injected()
        .into_iter()
        .filter_map(|i| match i {
            Injected::Mouse(MouseEvent::Move { x, y, .. }) => Some((x, y)),
            _ => None,
        })
        .collect()
}

#[test]
fn edge_switch_forwards_input_absolute() {
    let (a, ma, b, mb) = pair(false);
    assert_eq!(a.status().peers[0].id, b.machine_id());

    // Cursor hits A's right edge -> B is to the right.
    assert_eq!(ma.fire(InputEvent::Mouse(MouseEvent::Move { x: 1919, y: 500, dx: 1, dy: 0 })), Disposition::Pass);
    assert_eq!(a.active_machine(), "B");
    assert_eq!(ma.move_by(7, 0), Disposition::Swallow);
    ma.move_by(20, 0);
    assert_eq!(ma.fire(InputEvent::Key(KeyEvent { vk: 0x41, down: true, extended: false })), Disposition::Swallow);
    ma.fire(InputEvent::Key(KeyEvent { vk: 0x41, down: false, extended: false }));
    ma.fire(InputEvent::Mouse(MouseEvent::Wheel { delta: 120 }));

    assert!(wait_until(Duration::from_secs(5), || mb.injected().len() >= 5), "B got {:?}", mb.injected());
    std::thread::sleep(Duration::from_millis(200)); // duplicates (2 sockets) would arrive now
    // Each package arrives on both sockets; de-duplication by package id must drop the copy.
    assert_eq!(moves(&mb), vec![(0, 498), (20, 498)]);
    let rest: Vec<_> = mb.injected().into_iter().filter(|i| !matches!(i, Injected::Mouse(MouseEvent::Move { .. }))).collect();
    assert_eq!(
        rest,
        vec![
            Injected::Key(KeyEvent { vk: 0x41, down: true, extended: false }),
            Injected::Key(KeyEvent { vk: 0x41, down: false, extended: false }),
            Injected::Mouse(MouseEvent::Wheel { delta: 120 }),
        ]
    );
    assert!(wait_until(Duration::from_secs(2), || b.status().controlled_by.as_deref() == Some("A")));

    // Back across the left edge of B -> control returns to A.
    std::thread::sleep(Duration::from_millis(150));
    ma.move_by(-200, 0);
    assert_eq!(a.active_machine(), "A");
    assert!(ma.cursor().0 >= 1915);
    assert!(wait_until(Duration::from_secs(3), || b.status().controlled_by.is_none()), "HideMouse not processed");
    assert_eq!(mb.cursor(), (960, 540));
    // Local input passes again.
    assert_eq!(ma.move_by(-5, 0), Disposition::Pass);

    // Clipboard A -> B
    let ev = b.events().unwrap();
    a.send_clipboard_text("shared text").unwrap();
    let got = loop {
        match ev.recv_timeout(Duration::from_secs(5)).unwrap() {
            mwb::EngineEvent::ClipboardText { text, .. } => break text,
            _ => {}
        }
    };
    assert_eq!(got, "shared text");

    // ByeBye on stop.
    a.stop();
    assert!(wait_until(Duration::from_secs(5), || b.status().peers.is_empty()));
    b.stop();
}

#[test]
fn relative_mode_next_machine() {
    let (a, ma, b, mb) = pair(true);
    ma.fire(InputEvent::Mouse(MouseEvent::Move { x: 1919, y: 500, dx: 1, dy: 0 }));
    assert_eq!(a.active_machine(), "B");
    ma.move_by(3, 0); // first package after a switch is absolute (switch location)
    assert!(wait_until(Duration::from_secs(5), || moves(&mb).len() == 1));
    ma.move_by(30, 2); // relative +30,+2
    assert!(wait_until(Duration::from_secs(5), || moves(&mb).len() == 2), "{:?}", mb.injected());
    assert_eq!(moves(&mb), vec![(0, 498), (30, 500)]);

    // B detects its own left edge and asks A (NextMachine) to take control back.
    std::thread::sleep(Duration::from_millis(150));
    ma.move_by(-50, 0);
    assert!(wait_until(Duration::from_secs(5), || a.active_machine() == "A"), "NextMachine not handled");
    assert!(ma.cursor().0 >= 1915);
    assert!(!ma.cursor_locked());
    assert!(wait_until(Duration::from_secs(3), || b.status().controlled_by.is_none()));
    a.stop();
    b.stop();
}

#[test]
fn wrong_key_never_trusts() {
    let (pa, pb) = (free_base_port(), free_base_port());
    let a = Engine::start_with(cfg("A", pa, "B", pb, KEY), MockBackend::hd(), no_scan()).unwrap();
    let b = Engine::start_with(cfg("B", pb, "A", pa, "zzzzYYYY9999!!!!"), MockBackend::hd(), no_scan()).unwrap();
    assert!(!wait_until(Duration::from_secs(4), || !a.status().peers.is_empty() || !b.status().peers.is_empty()));
}

#[test]
fn switch_to_api() {
    let (a, ma, b, _mb) = pair(false);
    assert!(a.switch_to("b"));
    assert_eq!(a.active_machine(), "B");
    assert!(ma.cursor_locked());
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(a.switch_to("A"));
    assert_eq!(a.active_machine(), "A");
    assert!(!ma.cursor_locked());
    assert!(!a.switch_to("nobody"));
    a.stop();
    b.stop();
}

// ---------------------------------------------------------------------------
// Discovery (only injected loopback candidates: these tests never scan a network)

const OTHER_KEY: &str = "zzzzYYYY9999!!!!";

/// A config that only knows itself: no peers, just this machine in the layout.
fn lone(name: &str, port: u16, key: &str, auto: bool) -> Config {
    Config {
        key: key.into(),
        machine_name: name.into(),
        matrix: vec![name.into()],
        port,
        bind_address: "127.0.0.1".into(),
        swap_cmd_ctrl: false,
        reconnect_secs: 1,
        auto_discover: auto,
        ..Default::default()
    }
}

fn msg_addr(base_port: u16) -> SocketAddr {
    format!("127.0.0.1:{}", base_port + 1).parse().unwrap()
}

/// Wait for an event matching `f`, keeping the others in `seen`.
fn wait_event(rx: &Receiver<EngineEvent>, seen: &mut Vec<EngineEvent>, timeout: Duration, f: impl Fn(&EngineEvent) -> bool) -> Option<EngineEvent> {
    if let Some(e) = seen.iter().find(|e| f(e)) {
        return Some(e.clone());
    }
    let end = Instant::now() + timeout;
    while let Ok(e) = rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
        seen.push(e.clone());
        if f(&e) {
            return Some(e);
        }
    }
    None
}

fn slots(names: &[&str]) -> [String; 4] {
    std::array::from_fn(|i| names.get(i).map(|s| s.to_string()).unwrap_or_default())
}

#[test]
fn discovers_peer_through_injected_candidate() {
    let (pa, pb) = (free_base_port(), free_base_port());
    // B only answers (it would add an unknown machine that connects with the right key).
    let b = Engine::start_with(lone("B", pb, KEY, true), MockBackend::hd(), no_scan()).unwrap();
    let b_events = b.events().unwrap();
    let a = Engine::start_with(lone("A", pa, KEY, true), MockBackend::hd(), Arc::new(StaticCandidates(vec![msg_addr(pb)]))).unwrap();
    let ev = a.events().unwrap();
    let mut seen = Vec::new();

    let found = wait_event(&ev, &mut seen, Duration::from_secs(15), |e| matches!(e, EngineEvent::PeerDiscovered { .. }))
        .expect("PeerDiscovered");
    assert_eq!(found, EngineEvent::PeerDiscovered { name: "B".into(), id: b.machine_id(), addr: msg_addr(pb) });
    // New machines go to the right of this one.
    let m = wait_event(&ev, &mut seen, Duration::from_secs(5), |e| matches!(e, EngineEvent::MatrixChanged(_))).expect("MatrixChanged");
    let EngineEvent::MatrixChanged(m) = m else { unreachable!() };
    assert_eq!(m.slots, slots(&["A", "B"]));
    assert!(seen.iter().any(|e| matches!(e, EngineEvent::PeerConnected { name, .. } if name == "B")));
    let st = a.status();
    assert_eq!(st.discovered.len(), 1);
    assert_eq!(st.discovered[0].name, "B");
    assert_eq!(st.matrix.slots, slots(&["A", "B"]));

    // B learnt A too, adopts A's layout (broadcast after the discovery), and connects back.
    assert!(
        wait_until(Duration::from_secs(10), || b.status().matrix.slots == slots(&["A", "B"])),
        "B's layout: {:?}",
        b.status().matrix
    );
    assert!(wait_until(Duration::from_secs(10), || {
        let (sa, sb) = (a.status(), b.status());
        sa.peers.len() == 1 && sb.peers.len() == 1 && sa.peers[0].name == "B" && sb.peers[0].name == "A"
    }));
    assert_eq!(a.status().peers[0].ip, Some("127.0.0.1".parse().unwrap()));
    let mut b_seen = Vec::new();
    assert!(wait_event(&b_events, &mut b_seen, Duration::from_secs(2), |e| matches!(e, EngineEvent::PeerDiscovered { name, .. } if name == "A")).is_some());

    // Switching works across the discovered link.
    assert!(a.switch_to("B"));
    std::thread::sleep(Duration::from_millis(150));
    assert!(a.switch_to("A"));

    // The discovered address is used for reconnects: restart B on the same
    // port; A reconnects long before its next sweep (60 s). The generous
    // timeout covers slow CI runners (debug-build PBKDF2 per handshake).
    b.stop();
    drop(b);
    assert!(wait_until(Duration::from_secs(5), || a.status().peers.is_empty()));
    let b = Engine::start_with(lone("B", pb, KEY, false), MockBackend::hd(), no_scan()).unwrap();
    assert!(
        wait_until(Duration::from_secs(30), || a.status().peers.iter().any(|p| p.name == "B")),
        "A did not reconnect to B through the discovered address"
    );
    a.stop();
    b.stop();
}

#[test]
fn wrong_key_candidate_is_ignored_and_backed_off() {
    let (pa, pb) = (free_base_port(), free_base_port());
    let b = Engine::start_with(lone("B", pb, OTHER_KEY, false), MockBackend::hd(), no_scan()).unwrap();
    let a = Engine::start_with(lone("A", pa, KEY, true), MockBackend::hd(), Arc::new(StaticCandidates(vec![msg_addr(pb)]))).unwrap();
    let ev = a.events().unwrap();
    let mut seen = Vec::new();

    let first = wait_event(&ev, &mut seen, Duration::from_secs(15), |e| matches!(e, EngineEvent::DiscoveryFinished { .. }));
    assert_eq!(first, Some(EngineEvent::DiscoveryFinished { probed: 1, open: 1 }));
    assert!(
        wait_until(Duration::from_secs(10), || a.status().other_key == vec![msg_addr(pb)]),
        "not backed off: {:?}",
        a.status().other_key
    );
    // Not trusted, not added.
    let st = a.status();
    assert!(st.peers.is_empty() && st.discovered.is_empty());
    assert_eq!(st.matrix.slots, slots(&["A"]));
    assert!(b.status().peers.is_empty());
    assert!(!seen.iter().any(|e| matches!(e, EngineEvent::PeerDiscovered { .. } | EngineEvent::MatrixChanged(_))));

    // The next sweep leaves the host alone.
    seen.clear();
    a.discover_now();
    let second = wait_event(&ev, &mut seen, Duration::from_secs(5), |e| matches!(e, EngineEvent::DiscoveryFinished { .. }));
    assert_eq!(second, Some(EngineEvent::DiscoveryFinished { probed: 0, open: 0 }));
    a.stop();
    b.stop();
}

#[test]
fn discovery_off_means_no_sweep_until_asked() {
    let (pa, pb) = (free_base_port(), free_base_port());
    let b = Engine::start_with(lone("B", pb, KEY, false), MockBackend::hd(), no_scan()).unwrap();
    let a = Engine::start_with(lone("A", pa, KEY, false), MockBackend::hd(), Arc::new(StaticCandidates(vec![msg_addr(pb)]))).unwrap();
    let ev = a.events().unwrap();
    let mut seen = Vec::new();
    assert!(wait_event(&ev, &mut seen, Duration::from_secs(3), |e| matches!(e, EngineEvent::DiscoveryStarted)).is_none());
    assert!(a.status().peers.is_empty());
    a.discover_now();
    assert!(wait_event(&ev, &mut seen, Duration::from_secs(10), |e| matches!(e, EngineEvent::PeerDiscovered { name, .. } if name == "B")).is_some());
    a.stop();
    b.stop();
}

#[test]
fn live_peer_and_layout_updates() {
    let (pa, pb) = (free_base_port(), free_base_port());
    // A starts without B's address: nothing to connect to.
    let mut ca = lone("A", pa, KEY, false);
    ca.matrix = vec!["A".into(), "B".into()];
    ca.peers.insert("B".into(), "127.0.0.1:1".into()); // closed port; no DNS lookups
    let a = Engine::start_with(ca.clone(), MockBackend::hd(), no_scan()).unwrap();
    let b = Engine::start_with(cfg("B", pb, "A", pa, KEY), MockBackend::hd(), no_scan()).unwrap();
    // B knows A's address, so they connect anyway; A learns about B from B's
    // connection. Give A B's address without a restart and check the layout too.
    let mut c2 = ca.clone();
    c2.peers.insert("B".into(), format!("127.0.0.1:{pb}"));
    assert!(a.update_config(&c2));
    assert!(wait_until(Duration::from_secs(10), || a.status().peers.first().is_some_and(|p| p.connections == 2)));
    let mut c3 = c2.clone();
    c3.matrix = vec!["B".into(), "A".into()];
    assert!(a.update_config(&c3));
    assert_eq!(a.status().matrix.slots, slots(&["B", "A"]));
    // The new layout was sent to B (MWB semantics), which adopts it.
    assert!(wait_until(Duration::from_secs(5), || b.status().matrix.slots == slots(&["B", "A"])));
    // Socket settings need a restart.
    let mut c4 = c3.clone();
    c4.port = pa + 10;
    assert!(!a.update_config(&c4));
    a.stop();
    b.stop();
}
