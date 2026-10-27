//! Pairing the way a real PowerToys PC does it: the C# harness's `pt` mode
//! (interop/PowerToysFlow.cs) ports PowerToys' Connect path - Settings UI ->
//! ConnectToMachine -> machine matrix / pool -> StartNewTcpClient (IP mapping,
//! Dns.GetHostEntry) -> MainTCPRoutine -> AddToMachinePool - and is driven
//! exactly like the Connect button. "127.0.0.1" stands in for the Mac's LAN
//! address; the PC's DNS is an in-memory table (no network traffic).

mod common;

use std::time::Duration;

use common::*;
use mwb::mock::{Injected, MockBackend};
use mwb::{Config, Engine, EngineEvent};
use pm_input::{InputEvent, KeyEvent, MouseButton, MouseEvent};
use serde_json::Value;

const MAC: &str = "MacBook-Pro";
const PC: &str = "DESKTOP-PC";
const PC_ID: &str = "7777";

fn mac_cfg(port: u16) -> Config {
    Config {
        key: KEY.into(),
        machine_name: MAC.into(),
        matrix: vec![MAC.into()],
        port,
        bind_address: "127.0.0.1".into(),
        swap_cmd_ctrl: false,
        reconnect_secs: 1,
        auto_discover: false,
        answer_name_queries: false,
        // Never resolve the PC's name by DNS in tests: a closed local port.
        peers: [(PC.to_string(), "127.0.0.1:1".to_string())].into(),
        ..Default::default()
    }
}

fn ev(v: &Value, name: &str) -> bool {
    v["event"] == name
}

fn pc(port: u16, extra: &[&str]) -> Peer {
    let p = port.to_string();
    let mut args = vec!["pt", p.as_str(), PC, PC_ID, "40"];
    args.extend_from_slice(extra);
    let mut peer = Peer::spawn(&args).unwrap();
    peer.wait_for(Duration::from_secs(10), |v| ev(v, "ready"));
    peer
}

/// The field report: "Connect" with the Mac's IP address. PowerToys puts
/// "10.0.0.112" in the matrix and resolves it with Dns.GetHostEntry, which for
/// an address string does a reverse lookup first; without a PTR record it
/// throws, and no TCP connection is ever attempted.
#[test]
fn connect_with_an_ip_address_never_dials() {
    if harness().is_none() {
        return;
    }
    let port = free_base_port();
    let mac = Engine::start_with(mac_cfg(port), MockBackend::hd(), no_scan()).unwrap();
    let mut pc = pc(port, &[]);
    pc.send(&format!("connect 127.0.0.1 {KEY}"));
    let c = pc.wait_for(Duration::from_secs(5), |v| ev(v, "connect_to_machine"));
    assert_eq!(c["matrix"][0], "127.0.0.1");
    assert_eq!(c["matrix"][1], PC);
    pc.wait_for(Duration::from_secs(5), |v| ev(v, "dns_error") && v["name"] == "127.0.0.1");
    pc.wait_for(Duration::from_secs(5), |v| ev(v, "no_address"));
    pc.drain_for(Duration::from_millis(1500));
    assert!(!pc.seen.iter().any(|v| ev(v, "connecting")), "{:#?}", pc.seen);
    assert!(mac.status().incoming.is_empty(), "the Mac saw a connection: {:?}", mac.status().incoming);
    mac.stop();
}

/// The working setup: Connect with the Mac's *name* (resolvable through the IP
/// address mapping here, LLMNR on a real network). The PC firewalls inbound
/// connections (no listener), so the single PC->Mac socket carries everything:
/// input both ways, clipboard both ways and the layout both ways.
#[test]
fn connect_by_name_pairs_over_one_socket() {
    if harness().is_none() {
        return;
    }
    let port = free_base_port();
    let mock = MockBackend::hd();
    let mac = Engine::start_with(mac_cfg(port), mock.clone(), no_scan()).unwrap();
    let events = mac.events().unwrap();
    let mut pc = pc(port, &[]);
    // "IP address mapping" box: one "NAME IP" per line.
    pc.send("name2ip MACBOOK-PRO 127.0.0.1");
    pc.wait_for(Duration::from_secs(5), |v| ev(v, "ok"));
    // Typed as shown on the Mac; PowerToys upper-cases it.
    pc.send(&format!("connect {MAC} {KEY}"));
    let c = pc.wait_for(Duration::from_secs(5), |v| ev(v, "connect_to_machine"));
    assert_eq!(c["matrix"][0], "MACBOOK-PRO");
    pc.wait_for(Duration::from_secs(5), |v| ev(v, "connecting") && v["ip"] == "127.0.0.1");
    let t = pc.wait_for(Duration::from_secs(20), |v| ev(v, "trusted"));
    assert_eq!(t["client"], true);
    assert_eq!(t["dialed_name"], "MACBOOK-PRO");
    assert_eq!(t["claimed_name"], MAC);
    assert_eq!(t["remote_id"].as_u64().unwrap() as u32, mac.machine_id());
    // The claimed name matches the matrix entry (case-insensitively), so the
    // pool entry is updated in place: no rebuilt matrix, no socket reopen.
    assert_eq!(t["in_pool"], true);
    assert_eq!(t["in_matrix"], true);

    // The Mac records the attempt, trusts the PC and puts it next to itself.
    assert!(wait_until(Duration::from_secs(10), || mac.status().peers.iter().any(|p| p.name == PC && p.id == 7777)));
    let st = mac.status();
    assert_eq!(st.incoming.len(), 1, "{:?}", st.incoming);
    assert_eq!(st.incoming[0].result, format!("trusted as '{PC}'"));
    assert_eq!(st.matrix.slots[..2], [MAC.to_string(), PC.to_string()]);
    pc.drain_for(Duration::from_secs(2));
    assert!(!pc.seen.iter().any(|v| ev(v, "pool_learned") || ev(v, "closed")), "{:#?}", pc.seen);
    // Our client connection to the PC fails (nothing listens there), the
    // PC->Mac socket stays the only one.
    assert_eq!(mac.status().peers[0].connections, 1);

    // PC -> Mac input (SwitchToMachine("MACBOOK-PRO") resolves through the pool).
    pc.send("switch MACBOOK-PRO");
    pc.wait_for(Duration::from_secs(5), |v| ev(v, "switch_done"));
    assert!(wait_until(Duration::from_secs(5), || mock.injected().len() >= 5), "{:?}", mock.injected());
    assert_eq!(
        mock.injected()[..5],
        [
            Injected::Mouse(MouseEvent::Move { x: 960, y: 270, dx: 0, dy: 0 }),
            Injected::Mouse(MouseEvent::Button { button: MouseButton::Left, down: true, x: 960, y: 270 }),
            Injected::Mouse(MouseEvent::Button { button: MouseButton::Left, down: false, x: 960, y: 270 }),
            Injected::Key(KeyEvent { vk: 0x42, down: true, extended: false }),
            Injected::Key(KeyEvent { vk: 0x42, down: false, extended: false }),
        ]
    );

    // Mac -> PC input: right edge of the Mac switches to the PC.
    std::thread::sleep(Duration::from_millis(200));
    mock.fire(InputEvent::Mouse(MouseEvent::Move { x: 1919, y: 500, dx: 1, dy: 0 }));
    assert_eq!(mac.active_machine(), PC);
    mock.move_by(10, 0);
    mock.move_by(5, 4);
    mock.fire(InputEvent::Key(KeyEvent { vk: 0x41, down: true, extended: false }));
    let p = |v: &Value, t: i64| ev(v, "packet") && v["p"]["type"] == t && v["p"]["des"] == 7777;
    pc.wait_for(Duration::from_secs(5), |v| p(v, 123) && v["p"]["flags"] == 0x200);
    pc.wait_for(Duration::from_secs(5), |v| p(v, 122) && v["p"]["vk"] == 0x41);
    std::thread::sleep(Duration::from_millis(150));
    mock.move_by(-200, 0);
    assert_eq!(mac.active_machine(), MAC);

    // Clipboard both ways.
    pc.send("clip from the PC ✓");
    let clip = loop {
        match events.recv_timeout(Duration::from_secs(5)).expect("clipboard") {
            EngineEvent::ClipboardText { from, text } => break (from, text),
            _ => continue,
        }
    };
    assert_eq!(clip, (PC.to_string(), "from the PC ✓".to_string()));
    mac.send_clipboard_text("from the Mac ✓").unwrap();
    assert_eq!(pc.wait_for(Duration::from_secs(5), |v| ev(v, "clipboard"))["text"], "from the Mac ✓");

    // Layout from the PC (its layout editor) is adopted...
    pc.send("matrix DESKTOP-PC,MACBOOK-PRO,,");
    assert!(
        wait_until(Duration::from_secs(5), || mac.status().matrix.slots[..2] == [PC.to_string(), "MACBOOK-PRO".to_string()]),
        "{:?}",
        mac.status().matrix
    );
    // ...and ours reaches the PC.
    mac.set_matrix(&[MAC.into(), PC.into()], true, false);
    let m = pc.wait_for(Duration::from_secs(5), |v| ev(v, "matrix_received") && v["matrix"][0] == MAC);
    assert_eq!(m["matrix"][1], PC);
    mac.stop();
}

/// Connect with the IP address *and* a mapping for that address: the PC
/// dials, but the Mac's handshake says "MacBook-Pro", which isn't in the PC's
/// matrix. The Mac's first heartbeat makes PowerToys learn a "new" machine
/// (AddToMachinePool), rebuild its matrix and reopen every socket, and it then
/// redials by *name* - so the name has to resolve anyway.
#[test]
fn connect_with_ip_and_mapping_for_the_ip_redials_by_name() {
    if harness().is_none() {
        return;
    }
    let port = free_base_port();
    let mac = Engine::start_with(mac_cfg(port), MockBackend::hd(), no_scan()).unwrap();
    let mut pc = pc(port, &[]);
    pc.send("name2ip 127.0.0.1 127.0.0.1");
    pc.wait_for(Duration::from_secs(5), |v| ev(v, "ok"));
    pc.send(&format!("connect 127.0.0.1 {KEY}"));
    let t = pc.wait_for(Duration::from_secs(20), |v| ev(v, "trusted"));
    assert_eq!(t["dialed_name"], "127.0.0.1");
    assert_eq!(t["claimed_name"], MAC);
    assert_eq!(t["in_pool"], false);
    let l = pc.wait_for(Duration::from_secs(10), |v| ev(v, "pool_learned"));
    assert_eq!(l["name"], MAC);
    pc.wait_for(Duration::from_secs(5), |v| ev(v, "reopen_sockets") && v["by_user"] == true);
    // Redial by the learned name: no mapping or DNS answer for it -> nothing.
    pc.wait_for(Duration::from_secs(10), |v| ev(v, "no_address") && v["name"] == MAC);
    mac.stop();
}

/// When the PC accepts connections too, the Mac dials back after the PC
/// connected to it, and both sockets are kept (MWB's usual pair).
#[test]
fn listening_pc_gets_the_second_socket() {
    if harness().is_none() {
        return;
    }
    let port = free_base_port();
    let pc_port = free_base_port();
    let mut cfg = mac_cfg(port);
    // Where the PC listens (it can't share our port on 127.0.0.1).
    cfg.peers.insert(PC.into(), format!("127.0.0.1:{pc_port}"));
    let mac = Engine::start_with(cfg, MockBackend::hd(), no_scan()).unwrap();
    let listen = (pc_port + 1).to_string();
    let mut pc = pc(port, &["--listen", &listen]);
    pc.send("name2ip MACBOOK-PRO 127.0.0.1");
    pc.send(&format!("connect MACBOOK-PRO {KEY}"));
    pc.wait_for(Duration::from_secs(20), |v| ev(v, "trusted") && v["client"] == true);
    // The Mac adds the PC to its layout and connects to it.
    pc.wait_for(Duration::from_secs(20), |v| ev(v, "trusted") && v["client"] == false && v["claimed_name"] == MAC);
    assert!(
        wait_until(Duration::from_secs(10), || mac.status().peers.first().is_some_and(|p| p.connections == 2)),
        "{:?}",
        mac.status().peers
    );
    pc.send("status");
    let s = pc.wait_for(Duration::from_secs(5), |v| ev(v, "status"));
    let connected = s["sockets"].as_array().unwrap().iter().filter(|t| t["status"] == "Connected").count();
    assert_eq!(connected, 2, "{s:#}");
    // Input still flows (whichever socket carries it).
    pc.send("switch MACBOOK-PRO");
    pc.wait_for(Duration::from_secs(5), |v| ev(v, "switch_done"));
    mac.stop();
}
