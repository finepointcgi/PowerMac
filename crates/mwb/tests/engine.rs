//! End-to-end: two Rust engines on localhost with mock backends.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use mwb::mock::{Injected, MockBackend};
use mwb::{Config, Engine, EngineHandle};
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
    let a = Engine::start(ca, ma.clone()).unwrap();
    let b = Engine::start(cb, mb.clone()).unwrap();
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
    let a = Engine::start(cfg("A", pa, "B", pb, KEY), MockBackend::hd()).unwrap();
    let b = Engine::start(cfg("B", pb, "A", pa, "zzzzYYYY9999!!!!"), MockBackend::hd()).unwrap();
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
