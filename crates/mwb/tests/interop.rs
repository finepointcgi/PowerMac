//! Wire compatibility against the real C# MWB code (copied into interop/MwbCore.cs).

mod common;

use std::collections::HashMap;
use std::time::Duration;

use common::*;
use mwb::mock::{Injected, MockBackend};
use mwb::protocol::{self, ptype, Data, ID_ALL};
use mwb::transport::{SecureReader, SecureWriter};
use mwb::{clipboard, crypto, Config, Engine, EngineEvent};
use pm_input::{InputEvent, KeyEvent, MouseButton, MouseEvent};

fn rust_samples() -> HashMap<&'static str, Data> {
    let mut m = HashMap::new();
    let mut d = Data::new(ptype::Mouse);
    d.set_id(0x01020304);
    d.set_src(0x11223344);
    d.set_des(ID_ALL);
    d.set_md_x(12345);
    d.set_md_y(-54321);
    d.set_md_wheel(120);
    d.set_md_flags(0x20A);
    m.insert("mouse", d);

    let mut d = Data::new(ptype::Keyboard);
    d.set_id(7);
    d.set_src(5);
    d.set_des(6);
    d.set_date_time(0x0102030405060708);
    d.set_kd_vk(0x41);
    d.set_kd_flags(0x81);
    m.insert("keyboard", d);

    let mut d = Data::new(ptype::Hello);
    d.set_id(9);
    d.set_src(5);
    d.set_des(ID_ALL);
    d.set_machine_name("MAC-MINI");
    m.insert("hello", d);

    let mut d = Data::new(ptype::Matrix | ptype::MatrixSwapFlag | ptype::MatrixTwoRowFlag);
    d.set_src(4);
    d.set_des(ID_ALL);
    d.set_machine_name("WIN-PC");
    m.insert("matrix", d);

    let buf: Vec<u8> = (0..64).map(|i| (i * 7 + 3) as u8).collect();
    let mut hs = Data::from_bytes(&buf);
    hs.set_machine_name("HOSTX");
    hs.set_type(ptype::Handshake);
    m.insert("handshake", hs.clone());

    let mut ack = hs.clone();
    ack.set_type(ptype::HandshakeAck);
    ack.set_src(0);
    ack.set_machine_name("REPLIER");
    for i in 0..4 {
        ack.set_machine(i, !ack.machine(i));
    }
    m.insert("handshake_ack", ack);

    let mut d = Data::new(ptype::NextMachine);
    d.set_des(77);
    d.set_md_x(100);
    d.set_md_y(200);
    d.set_md_wheel(0x12345);
    m.insert("next_machine", d);

    let mut d = Data::new(ptype::ByeBye);
    d.set_des(ID_ALL);
    d.set_machine_name("IGNORED-SMALL");
    m.insert("byebye", d);
    m
}

#[test]
fn csharp_test_vectors() {
    let Some(v) = run_harness(&["vectors"]) else { return };

    // Get24BitHash
    for h in v["hashes"].as_array().unwrap() {
        let key = h["key"].as_str().unwrap();
        assert_eq!(crypto::get_24bit_hash(key) as u64, h["hash"].as_u64().unwrap(), "hash of {key:?}");
    }

    // PBKDF2
    let dk = &v["derived_key"];
    let salt = hex(dk["salt"].as_str().unwrap());
    assert_eq!(to_hex(&crypto::derive_key(dk["key"].as_str().unwrap(), &salt)), dk["dk"].as_str().unwrap());

    // DATA serialization + stamping
    let magic = v["magic"].as_u64().unwrap() as u32;
    let samples = rust_samples();
    for p in v["packets"].as_array().unwrap() {
        let name = p["name"].as_str().unwrap();
        let d = &samples[name];
        let cs_bytes = hex(p["bytes"].as_str().unwrap());
        assert_eq!(to_hex(d.wire_bytes().unwrap()), to_hex(&cs_bytes), "DATA bytes for {name}");
        let mut st = cs_bytes.clone();
        protocol::stamp(&mut st, magic);
        assert_eq!(to_hex(&st), p["stamped"].as_str().unwrap(), "stamped {name}");
        let mut chk = [0u8; 64];
        chk[..st.len()].copy_from_slice(&st);
        assert_eq!(protocol::validate_received(&mut chk, magic), p["valid"].as_bool().unwrap());
        // decode C# bytes back
        let back = Data::from_bytes(&cs_bytes);
        assert_eq!(back.ptype(), d.ptype());
        if back.is_big() {
            assert_eq!(back.machine_name(), d.machine_name());
        }
    }

    // Encrypted stream, byte-exact with CryptoStream.
    let key = v["stream_key"].as_str().unwrap();
    let header: Vec<u8> = (1..=32).collect();
    let first: [u8; 16] = std::array::from_fn(|i| 0xA0 + i as u8);
    let dk = crypto::derive_key(key, &header[..16]);
    let mut out = Vec::new();
    {
        let mut w = SecureWriter::with_params(&mut out, &dk, header[..16].try_into().unwrap(), header[16..].try_into().unwrap(), &first).unwrap();
        w.send(&samples["mouse"], magic).unwrap();
        w.send(&samples["hello"], magic).unwrap();
    }
    let cs_stream = hex(v["stream"].as_str().unwrap());
    assert_eq!(to_hex(&out), to_hex(&cs_stream), "encrypted stream");
    let mut r = SecureReader::new(&cs_stream[..], key).unwrap();
    let a = r.recv(magic).unwrap();
    assert!(a.valid);
    assert_eq!(a.data.md_y(), -54321);
    let b = r.recv(magic).unwrap();
    assert_eq!(b.data.machine_name(), "MAC-MINI");

    // Clipboard (C# DeflateStream output, with GetBuffer() trailing bytes).
    let clip = &v["clip"];
    let t = clipboard::decode_text(&hex(clip["data"].as_str().unwrap())).unwrap();
    assert_eq!(t.text.as_deref(), clip["text"].as_str());

    // IsBigPackage for every type value.
    for e in v["is_big"].as_array().unwrap() {
        let t = e["t"].as_u64().unwrap() as u32;
        assert_eq!(protocol::is_big_package(t), Some(e["big"].as_bool().unwrap()), "is_big({t})");
    }
}

#[test]
fn csharp_decodes_rust_stream() {
    if harness().is_none() {
        return;
    }
    let key = KEY;
    let magic = crypto::get_24bit_hash(key);
    let mut out = Vec::new();
    let samples = rust_samples();
    {
        let mut w = SecureWriter::new(&mut out, key).unwrap();
        for n in ["mouse", "keyboard", "hello", "matrix", "next_machine", "byebye"] {
            w.send(&samples[n], magic).unwrap();
        }
    }
    let v = run_harness(&["decode-stream", key, &to_hex(&out)]).unwrap();
    let list = v.as_array().unwrap();
    assert_eq!(list.len(), 6);
    assert!(list.iter().all(|p| p["valid"] == true));
    assert_eq!(list[0]["type"], 123);
    assert_eq!(list[0]["x"], 12345);
    assert_eq!(list[0]["y"], -54321);
    assert_eq!(list[0]["flags"], 0x20A);
    assert_eq!(list[1]["vk"], 0x41);
    assert_eq!(list[1]["kflags"], 0x81);
    assert_eq!(list[2]["name"], "MAC-MINI");
    assert_eq!(list[3]["type"], 134);
    assert_eq!(list[3]["name"], "WIN-PC");
    assert_eq!(list[4]["wheel"], 0x12345);
    assert_eq!(list[5]["type"], 4);

    let payload = clipboard::encode_text("Grüße from Rust ✓\nline 2");
    let v = run_harness(&["decode-clip", &to_hex(&payload)]).unwrap();
    assert_eq!(v["text"], "Grüße from Rust ✓\nline 2");
}

fn engine_cfg(name: &str, port: u16, matrix: &[&str]) -> Config {
    Config {
        key: KEY.into(),
        machine_name: name.into(),
        matrix: matrix.iter().map(|s| s.to_string()).collect(),
        port,
        bind_address: "127.0.0.1".into(),
        swap_cmd_ctrl: false,
        reconnect_secs: 1,
        auto_discover: false,
        ..Default::default()
    }
}

/// C# peer connects to the Rust engine and acts as the controller.
#[test]
fn csharp_controls_rust() {
    if harness().is_none() {
        return;
    }
    let port = free_base_port();
    let mock = MockBackend::hd();
    let mut cfg = engine_cfg("RUSTBOX", port, &["CSHARP", "RUSTBOX"]);
    cfg.peers.insert("CSHARP".into(), "127.0.0.1:1".into()); // don't try DNS
    let eng = Engine::start_with(cfg, mock.clone(), no_scan()).unwrap();
    let events = eng.events().unwrap();

    let mut peer = Peer::spawn(&["peer", "connect", "127.0.0.1", &port.to_string(), KEY, "CSHARP", "4242", "control", "20", "hi from C# ✓"]).unwrap();
    let trusted = peer.wait_for(Duration::from_secs(15), |v| v["event"] == "trusted");
    assert_eq!(trusted["remote_name"], "RUSTBOX");
    assert_eq!(trusted["remote_id"].as_u64().unwrap() as u32, eng.machine_id());
    // Rust sends its initial heartbeat with its name.
    peer.wait_for(Duration::from_secs(5), |v| v["event"] == "packet" && v["p"]["type"] == 20 && v["p"]["name"] == "RUSTBOX");
    peer.wait_for(Duration::from_secs(10), |v| v["event"] == "control_done");

    let clip = loop {
        match events.recv_timeout(Duration::from_secs(5)).expect("clipboard event") {
            EngineEvent::ClipboardText { from, text } => break (from, text),
            _ => continue,
        }
    };
    assert_eq!(clip, ("CSHARP".to_string(), "hi from C# ✓".to_string()));
    assert!(wait_until(Duration::from_secs(3), || mock.cursor() == (960, 540)), "HideMouse parks cursor");

    let inj = mock.injected();
    let want = vec![
        Injected::Mouse(MouseEvent::Move { x: 960, y: 270, dx: 0, dy: 0 }),
        Injected::Mouse(MouseEvent::Button { button: MouseButton::Left, down: true, x: 960, y: 270 }),
        Injected::Mouse(MouseEvent::Button { button: MouseButton::Left, down: false, x: 960, y: 270 }),
        Injected::Mouse(MouseEvent::Wheel { delta: -120 }),
        Injected::Mouse(MouseEvent::Move { x: 970, y: 265, dx: 0, dy: 0 }),
        Injected::Key(KeyEvent { vk: 0x41, down: true, extended: false }),
        Injected::Key(KeyEvent { vk: 0x41, down: false, extended: false }),
        Injected::Key(KeyEvent { vk: 0x25, down: true, extended: true }),
        Injected::Key(KeyEvent { vk: 0x25, down: false, extended: true }),
    ];
    assert_eq!(inj, want);
    let st = eng.status();
    assert_eq!(st.peers.len(), 1);
    assert_eq!(st.peers[0].name, "CSHARP");
    assert_eq!(st.peers[0].id, 4242);
    assert_eq!(st.controlled_by, None);
    eng.stop();
}

/// Rust engine connects to a C# peer and controls it via edge switching.
#[test]
fn rust_controls_csharp() {
    if harness().is_none() {
        return;
    }
    let cs_port = free_base_port();
    let mut peer = Peer::spawn(&["peer", "serve", "127.0.0.1", &cs_port.to_string(), KEY, "CSHARP", "5151", "listen", "25"]).unwrap();
    peer.wait_for(Duration::from_secs(10), |v| v["event"] == "listening");

    let mock = MockBackend::hd();
    let mut cfg = engine_cfg("RUSTBOX", free_base_port(), &["RUSTBOX", "CSHARP"]);
    cfg.peers.insert("CSHARP".into(), format!("127.0.0.1:{cs_port}"));
    let eng = Engine::start_with(cfg, mock.clone(), no_scan()).unwrap();
    let t = peer.wait_for(Duration::from_secs(15), |v| v["event"] == "trusted");
    assert_eq!(t["remote_name"], "RUSTBOX");
    assert!(wait_until(Duration::from_secs(5), || eng.connected_peers() == vec!["CSHARP".to_string()]));
    // C# initial heartbeat teaches us its id.
    assert!(wait_until(Duration::from_secs(5), || eng.status().peers.first().map(|p| p.id) == Some(5151)));
    std::thread::sleep(Duration::from_millis(200));

    // Hit the right edge -> switch to CSHARP.
    mock.fire(InputEvent::Mouse(MouseEvent::Move { x: 1919, y: 500, dx: 1, dy: 0 }));
    assert_eq!(eng.active_machine(), "CSHARP");
    assert_eq!(mock.cursor(), (960, 540));
    assert!(mock.cursor_locked());
    assert!(!mock.cursor_visible());
    assert_eq!(mock.move_by(10, 0), pm_input::Disposition::Swallow); // consumes the switch location
    mock.move_by(5, 4);
    assert_eq!(mock.fire(InputEvent::Key(KeyEvent { vk: 0x41, down: true, extended: false })), pm_input::Disposition::Swallow);
    mock.fire(InputEvent::Key(KeyEvent { vk: 0x41, down: false, extended: false }));
    mock.fire(InputEvent::Mouse(MouseEvent::Button { button: MouseButton::Right, down: true, x: 960, y: 540 }));
    mock.fire(InputEvent::Mouse(MouseEvent::Wheel { delta: 240 }));
    eng.send_clipboard_text("from rust ✓").unwrap();

    let p = |v: &serde_json::Value, t: i64| v["event"] == "packet" && v["p"]["type"] == t;
    peer.wait_for(Duration::from_secs(5), |v| p(v, 77) && v["p"]["des"] == 5151); // MachineSwitched
    let m1 = peer.wait_for(Duration::from_secs(5), |v| p(v, 123) && v["p"]["x"] == 34);
    assert_eq!(m1["p"]["des"], 5151);
    assert_eq!(m1["p"]["flags"], 0x200);
    assert_eq!(m1["p"]["y"], 499 * 65535 / 1080); // virt y = (500*65535/1080)*1080/65535 = 499
    let m2 = peer.wait_for(Duration::from_secs(5), |v| p(v, 123) && v["p"]["x"] == 6 * 65535 / 1920);
    assert_eq!(m2["p"]["y"], 503 * 65535 / 1080);
    peer.wait_for(Duration::from_secs(5), |v| p(v, 122) && v["p"]["vk"] == 0x41 && v["p"]["kflags"] == 0);
    peer.wait_for(Duration::from_secs(5), |v| p(v, 122) && v["p"]["vk"] == 0x41 && v["p"]["kflags"] == 0x80);
    peer.wait_for(Duration::from_secs(5), |v| p(v, 123) && v["p"]["flags"] == 0x204);
    peer.wait_for(Duration::from_secs(5), |v| p(v, 123) && v["p"]["flags"] == 0x20A && v["p"]["wheel"] == 240);
    let c = peer.wait_for(Duration::from_secs(5), |v| v["event"] == "clipboard");
    assert_eq!(c["text"], "from rust ✓");

    // Back to the left edge of the virtual cursor -> return to RUSTBOX.
    std::thread::sleep(Duration::from_millis(150)); // MWB ignores jumps within 100 ms of the last one
    mock.move_by(-100, 0);
    assert_eq!(eng.active_machine(), "RUSTBOX");
    assert!(!mock.cursor_locked());
    assert!(mock.cursor_visible());
    let (cx, cy) = mock.cursor();
    assert!(cx >= 1915 && (500..=506).contains(&cy), "cursor re-entered at right edge: {cx},{cy}");
    peer.wait_for(Duration::from_secs(5), |v| p(v, 50) && v["p"]["des"] == 5151); // HideMouse
    eng.stop();
    peer.wait_for(Duration::from_secs(5), |v| p(v, 4)); // ByeBye
}

/// Discovery finds the C# peer (PowerToys' own handshake code) through an
/// injected loopback candidate; the handshake succeeds and the layout is sent.
#[test]
fn discovers_csharp_peer() {
    if harness().is_none() {
        return;
    }
    let cs_port = free_base_port();
    let mut peer = Peer::spawn(&["peer", "serve", "127.0.0.1", &cs_port.to_string(), KEY, "CSHARP", "5151", "listen", "25"]).unwrap();
    peer.wait_for(Duration::from_secs(10), |v| v["event"] == "listening");

    let mut cfg = engine_cfg("RUSTBOX", free_base_port(), &["RUSTBOX"]);
    cfg.auto_discover = true;
    let cand: std::net::SocketAddr = format!("127.0.0.1:{}", cs_port + 1).parse().unwrap();
    let eng = Engine::start_with(cfg, MockBackend::hd(), std::sync::Arc::new(mwb::discovery::StaticCandidates(vec![cand]))).unwrap();
    let events = eng.events().unwrap();

    let t = peer.wait_for(Duration::from_secs(15), |v| v["event"] == "trusted");
    assert_eq!(t["remote_name"], "RUSTBOX");
    let found = loop {
        match events.recv_timeout(Duration::from_secs(10)).expect("PeerDiscovered") {
            EngineEvent::PeerDiscovered { name, addr, .. } => break (name, addr),
            _ => continue,
        }
    };
    assert_eq!(found, ("CSHARP".to_string(), cand));
    assert!(wait_until(Duration::from_secs(5), || eng.status().peers.first().map(|p| p.id) == Some(5151)));
    assert_eq!(eng.status().matrix.slots, ["RUSTBOX".to_string(), "CSHARP".into(), String::new(), String::new()]);
    // Our heartbeat reaches it (it is in the layout now), then the layout itself
    // (MachineStuff.SendMachineMatrix: one Matrix package per slot, src = slot).
    let p = |v: &serde_json::Value, t: i64| v["event"] == "packet" && v["p"]["type"] == t;
    peer.wait_for(Duration::from_secs(5), |v| p(v, 20) && v["p"]["name"] == "RUSTBOX");
    peer.wait_for(Duration::from_secs(5), |v| p(v, 128) && v["p"]["src"] == 1 && v["p"]["name"] == "RUSTBOX");
    peer.wait_for(Duration::from_secs(5), |v| p(v, 128) && v["p"]["src"] == 2 && v["p"]["name"] == "CSHARP");
    peer.wait_for(Duration::from_secs(5), |v| p(v, 128) && v["p"]["src"] == 4);
    eng.stop();
}

/// A C# peer with another key: the handshake fails on both sides and the
/// host is backed off instead of being probed again.
#[test]
fn csharp_peer_with_other_key_is_backed_off() {
    if harness().is_none() {
        return;
    }
    let cs_port = free_base_port();
    let mut peer = Peer::spawn(&["peer", "serve", "127.0.0.1", &cs_port.to_string(), "zzzzYYYY9999!!!!", "CSHARP", "5151", "listen", "25"]).unwrap();
    peer.wait_for(Duration::from_secs(10), |v| v["event"] == "listening");
    let mut cfg = engine_cfg("RUSTBOX", free_base_port(), &["RUSTBOX"]);
    cfg.auto_discover = true;
    let cand: std::net::SocketAddr = format!("127.0.0.1:{}", cs_port + 1).parse().unwrap();
    let eng = Engine::start_with(cfg, MockBackend::hd(), std::sync::Arc::new(mwb::discovery::StaticCandidates(vec![cand]))).unwrap();
    peer.wait_for(Duration::from_secs(15), |v| v["event"] == "invalid_key" || v["event"] == "error" || v["event"] == "closed");
    assert!(wait_until(Duration::from_secs(10), || eng.status().other_key == vec![cand]), "{:?}", eng.status().other_key);
    let st = eng.status();
    assert!(st.peers.is_empty() && st.discovered.is_empty());
    assert_eq!(st.matrix.slots[1], "");
    eng.stop();
}
