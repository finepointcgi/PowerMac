//! Runs the real `powermac-battery-helper` binary in `--fake-smc` test mode
//! (no root, no hardware writes) and talks to it over its Unix socket.
//!
//! The test chdirs into its working directory and uses relative socket
//! paths, so a long directory (over the 104-byte `sun_path` limit) still
//! works. Set `BATTERY_TEST_DIR` to choose the directory.
#![cfg(target_os = "macos")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Proc, Stdio};
use std::time::{Duration, Instant};

use battery_limit::helper::{send, status};
use battery_limit::protocol::Command;
use battery_limit::smc::FakeSmc;
use battery_limit::{BatteryLimit, Config, SystemEnvironment};

struct Helper(Child);

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(args: &[&str], log: &str) -> Helper {
    let log = std::fs::File::create(log).unwrap();
    let child = Proc::new(env!("CARGO_BIN_EXE_powermac-battery-helper"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    Helper(child)
}

fn wait_for_socket(p: &Path) {
    let t = Instant::now();
    while UnixStream::connect(p).is_err() {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "helper socket {} never appeared",
            p.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn key(state: &str, k: &str) -> String {
    battery_limit::smc::hex(&FakeSmc::open(Path::new(state)).unwrap().state.keys[k].bytes)
}

fn raw(sock: &Path, line: &str) -> String {
    let mut s = UnixStream::connect(sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out).unwrap();
    out.trim_end().to_string()
}

#[test]
fn helper_over_socket_with_fake_smc() {
    let dir = std::env::var_os("BATTERY_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("bl-helper-{}", std::process::id())));
    std::fs::create_dir_all(&dir).unwrap();
    // The other test uses absolute paths, so changing the cwd is safe.
    std::env::set_current_dir(&dir).unwrap();
    let sock = Path::new("h.sock");
    let state = "smc.json";

    // Start "inhibited" to check the startup restore.
    let mut f = FakeSmc::create(Path::new(state), "tahoe").unwrap();
    f.state.keys.get_mut("CHTE").unwrap().bytes = vec![1, 0, 0, 0];
    std::fs::write(state, serde_json::to_vec(&f.state).unwrap()).unwrap();

    let mut h = spawn(
        &[
            "--fake-smc",
            state,
            "--socket",
            "h.sock",
            "--watchdog-secs",
            "1.5",
        ],
        "helper.log",
    );
    wait_for_socket(sock);
    assert_eq!(key(state, "CHTE"), "00000000", "startup restores charging");

    // Socket permissions: 0600.
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(sock).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let r = send(sock, &[Command::Ping, Command::Status]).unwrap();
    assert_eq!(r[0], "OK PONG 2");
    let st = status(sock).unwrap();
    assert_eq!(st.scheme, "CHTE+CHIE");
    assert!(!st.inhibited && !st.discharging);

    // Commands drive the keys.
    send(sock, &[Command::Inhibit]).unwrap();
    assert_eq!(key(state, "CHTE"), "01000000");
    send(sock, &[Command::DischargeOn]).unwrap();
    assert_eq!(key(state, "CHIE"), "08");
    send(sock, &[Command::DischargeOff, Command::Charge]).unwrap();
    assert_eq!(
        (key(state, "CHTE"), key(state, "CHIE")),
        ("00000000".into(), "00".into())
    );

    // Nothing outside the command set.
    assert_eq!(raw(sock, "WRITE CHTE 01000000"), "ERR unknown command");
    assert_eq!(raw(sock, &"X".repeat(200)), "ERR line too long");

    // Watchdog: inhibit, then go quiet.
    send(sock, &[Command::Inhibit]).unwrap();
    assert_eq!(key(state, "CHTE"), "01000000");
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(key(state, "CHTE"), "00000000", "watchdog restored charging");
    assert!(!status(sock).unwrap().inhibited);

    // The real controller (real battery reading, fake SMC behind the helper).
    let env = SystemEnvironment::with_socket(sock, true);
    let ctl = BatteryLimit::start_with(
        Config {
            enabled: true,
            limit_percent: 20,
            ..Default::default()
        },
        env,
    )
    .unwrap();
    let t = Instant::now();
    let s = loop {
        let s = ctl.status();
        if s.helper_running || t.elapsed() > Duration::from_secs(5) {
            break s;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        s.helper_running && s.helper_installed && !s.needs_helper,
        "{s:?}"
    );
    assert_eq!(s.scheme.as_deref(), Some("CHTE+CHIE"));
    if s.battery.plugged && s.battery.percent.is_some_and(|p| p >= 20) {
        assert!(s.inhibited, "plugged in above a 20 % limit: {s:?}");
        assert_eq!(key(state, "CHTE"), "01000000");
    }
    ctl.stop();
    assert_eq!(key(state, "CHTE"), "00000000", "stop restores charging");

    // SIGTERM while inhibited + discharging: restored on exit, socket removed.
    send(sock, &[Command::Inhibit, Command::DischargeOn]).unwrap();
    // SAFETY: signalling our own child.
    unsafe { libc::kill(h.0.id() as i32, libc::SIGTERM) };
    let code = h.0.wait().unwrap();
    assert!(code.success(), "{code:?}");
    assert_eq!(
        (key(state, "CHTE"), key(state, "CHIE")),
        ("00000000".into(), "00".into())
    );
    assert!(!sock.exists());
    let writes = FakeSmc::open(Path::new(state)).unwrap().state.writes;
    assert!(
        writes.iter().all(|(k, _)| k == "CHTE" || k == "CHIE"),
        "{writes:?}"
    );

    // Peer-uid check: a helper that only admits root rejects us.
    let _h2 = spawn(
        &[
            "--fake-smc",
            state,
            "--socket",
            "h2.sock",
            "--allowed-uid",
            "0",
        ],
        "helper2.log",
    );
    wait_for_socket(Path::new("h2.sock"));
    assert_eq!(raw(Path::new("h2.sock"), "PING"), "ERR unauthorized");
    assert!(send(Path::new("h2.sock"), &[Command::Inhibit]).is_err());
    assert_eq!(key(state, "CHTE"), "00000000");
}

/// A Mac like the M4 Pro on macOS 27.2: inhibit keys gated, only CHIE.
#[test]
fn helper_on_gated_firmware_limits_via_discharge() {
    let dir = std::env::var_os("BATTERY_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("bl-helper-{}", std::process::id())))
        .join("gated");
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("g.sock");
    let state = dir.join("smc.json");
    let state_s = state.to_str().unwrap();
    // sun_path is 104 bytes; fall back to a short relative-free path check.
    assert!(
        sock.as_os_str().len() < 100,
        "set BATTERY_TEST_DIR to a short path"
    );
    let mut f = FakeSmc::create(&state, "gated").unwrap();
    f.state.keys.get_mut("CHIE").unwrap().bytes = vec![8];
    std::fs::write(&state, serde_json::to_vec(&f.state).unwrap()).unwrap();

    let mut h = spawn(
        &["--fake-smc", state_s, "--socket", sock.to_str().unwrap()],
        dir.join("helper.log").to_str().unwrap(),
    );
    wait_for_socket(&sock);
    assert_eq!(
        key(state_s, "CHIE"),
        "00",
        "startup turns the adapter back on"
    );
    let st = status(&sock).unwrap();
    assert_eq!(st.scheme, "none+CHIE");
    assert_eq!(st.version, 2);
    assert!(!st.can_inhibit() && st.range.is_none());

    assert!(send(&sock, &[Command::Inhibit]).is_err(), "no inhibit key");
    assert_eq!(raw(&sock, "RANGE 75 80").split(' ').next(), Some("OK"));
    assert_eq!(raw(&sock, "RANGE 80 75"), "ERR invalid range 80-75");
    send(&sock, &[Command::DischargeOn]).unwrap();
    assert_eq!(key(state_s, "CHIE"), "08");
    assert!(status(&sock).unwrap().discharging);

    // The real controller drives it: plugged in above a 20 % limit means
    // the adapter goes off ("limit via discharge").
    let ctl = BatteryLimit::start_with(
        Config {
            enabled: true,
            limit_percent: 20,
            ..Default::default()
        },
        SystemEnvironment::with_socket(&sock, true),
    )
    .unwrap();
    let t = Instant::now();
    let s = loop {
        let s = ctl.status();
        if s.helper_running || t.elapsed() > Duration::from_secs(5) {
            break s;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(s.strategy_label, "limit via discharge", "{s:?}");
    assert!(s.error.is_none(), "{s:?}");
    if s.battery.plugged && s.battery.percent.is_some_and(|p| p >= 20) {
        assert!(s.discharging, "{s:?}");
        assert_eq!(key(state_s, "CHIE"), "08");
    }
    ctl.stop();
    assert_eq!(key(state_s, "CHIE"), "00", "stop turns the adapter back on");

    send(&sock, &[Command::DischargeOn]).unwrap();
    // SAFETY: signalling our own child.
    unsafe { libc::kill(h.0.id() as i32, libc::SIGTERM) };
    assert!(h.0.wait().unwrap().success());
    assert_eq!(key(state_s, "CHIE"), "00");
    let writes = FakeSmc::open(&state).unwrap().state.writes;
    assert!(writes.iter().all(|(k, _)| k == "CHIE"), "{writes:?}");
}
