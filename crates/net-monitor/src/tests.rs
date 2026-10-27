use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use crate::model::*;
use crate::monitor::{Event, Monitor};
use crate::rules::*;

fn sa(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

fn proc_(pid: u32, path: &str) -> ProcessInfo {
    let p = PathBuf::from(path);
    ProcessInfo { pid, name: p.file_name().unwrap().to_string_lossy().into_owned(), path: Some(p) }
}

fn tcp(pid: u32, local: &str, remote: Option<&str>, state: TcpState) -> Socket {
    Socket { pid, protocol: Protocol::Tcp, local: sa(local), remote: remote.map(sa), state: Some(state) }
}

fn udp(pid: u32, local: &str, remote: Option<&str>) -> Socket {
    Socket { pid, protocol: Protocol::Udp, local: sa(local), remote: remote.map(sa), state: None }
}

const SAFARI: &str = "/Applications/Safari.app/Contents/MacOS/Safari";
const CHROME_HELPER: &str = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/1/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper";
const CURL: &str = "/usr/bin/curl";

fn sample() -> Snapshot {
    Snapshot::new(
        vec![proc_(10, SAFARI), proc_(20, CHROME_HELPER), proc_(21, "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"), proc_(30, CURL), proc_(40, "/usr/sbin/sshd")],
        vec![
            tcp(10, "192.168.1.5:50000", Some("17.253.144.10:443"), TcpState::Established),
            tcp(20, "192.168.1.5:50001", Some("142.250.1.1:443"), TcpState::Established),
            udp(21, "0.0.0.0:5353", None),
            tcp(30, "[::ffff:192.168.1.5]:50002", Some("[::ffff:93.184.215.14]:80"), TcpState::SynSent),
            tcp(40, "0.0.0.0:22", None, TcpState::Listen),
            tcp(40, "192.168.1.5:22", Some("192.168.1.9:61000"), TcpState::Established),
        ],
        0,
    )
}

// --- model -----------------------------------------------------------------

#[test]
fn bundles_are_outermost() {
    assert_eq!(outermost_bundle(Path::new(CHROME_HELPER)), Some(PathBuf::from("/Applications/Google Chrome.app")));
    assert_eq!(outermost_bundle(Path::new(SAFARI)), Some(PathBuf::from("/Applications/Safari.app")));
    assert_eq!(outermost_bundle(Path::new(CURL)), None);
    let p = proc_(20, CHROME_HELPER);
    assert_eq!(p.display_name(), "Google Chrome");
    assert_eq!(p.app_key(), "/Applications/Google Chrome.app");
    assert_eq!(proc_(30, CURL).display_name(), "curl");
}

#[test]
fn scopes() {
    let s = |x: &str| Scope::of(x.parse::<IpAddr>().unwrap());
    assert_eq!(s("127.0.0.1"), Scope::Loopback);
    assert_eq!(s("::1"), Scope::Loopback);
    assert_eq!(s("::ffff:127.0.0.1"), Scope::Loopback);
    assert_eq!(s("10.1.2.3"), Scope::LocalNetwork);
    assert_eq!(s("192.168.0.1"), Scope::LocalNetwork);
    assert_eq!(s("169.254.1.1"), Scope::LocalNetwork);
    assert_eq!(s("100.100.1.1"), Scope::LocalNetwork);
    assert_eq!(s("224.0.0.251"), Scope::LocalNetwork);
    assert_eq!(s("fe80::1"), Scope::LocalNetwork);
    assert_eq!(s("fd00::1"), Scope::LocalNetwork);
    assert_eq!(s("8.8.8.8"), Scope::Internet);
    assert_eq!(s("2606:4700::1111"), Scope::Internet);
    assert_eq!(s("100.128.0.1"), Scope::Internet);
}

#[test]
fn groups_by_app_and_infers_direction() {
    let snap = sample();
    let apps = snap.by_app();
    let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(apps.len(), 4, "{names:?}");
    let chrome = apps.iter().find(|a| a.name == "Google Chrome").unwrap();
    assert_eq!(chrome.pids, vec![20, 21]);
    assert_eq!(chrome.active(), 1);
    assert_eq!(chrome.listeners(), 1);
    let sshd = apps.iter().find(|a| a.name == "sshd").unwrap();
    assert!(sshd.connections.iter().all(|c| c.direction == Direction::Incoming));
    let safari = apps.iter().find(|a| a.name == "Safari").unwrap();
    assert_eq!(safari.connections[0].direction, Direction::Outgoing);
    let curl = apps.iter().find(|a| a.name == "curl").unwrap();
    assert_eq!(curl.remotes(), vec!["93.184.215.14".parse::<IpAddr>().unwrap()]);
    // Apps with connections come before ones that only listen.
    assert_ne!(names.last(), Some(&"Google Chrome"));
}

#[test]
fn describe_lines() {
    let s = tcp(1, "[::ffff:10.0.0.2]:5000", Some("[::ffff:1.1.1.1]:443"), TcpState::Established);
    assert_eq!(crate::describe(&s), "TCP [::ffff:10.0.0.2]:5000 → 1.1.1.1:443 ESTABLISHED");
    assert_eq!(crate::describe(&tcp(1, "0.0.0.0:22", None, TcpState::Listen)), "TCP 0.0.0.0:22 (listening)");
    assert_eq!(crate::describe(&udp(1, "0.0.0.0:53", None)), "UDP 0.0.0.0:53 (bound)");
}

#[test]
fn tcp_state_tables() {
    assert_eq!(TcpState::from_xnu(4), Some(TcpState::Established));
    assert_eq!(TcpState::from_xnu(1), Some(TcpState::Listen));
    assert_eq!(TcpState::from_xnu(11), None);
    assert_eq!(TcpState::from_mib(5), Some(TcpState::Established));
    assert_eq!(TcpState::from_mib(2), Some(TcpState::Listen));
    assert_eq!(TcpState::from_mib(7), Some(TcpState::FinWait2));
    assert_eq!(TcpState::from_mib(12), Some(TcpState::Closed));
}

#[test]
fn snapshot_json_roundtrip() {
    let snap = sample();
    let j = serde_json::to_string(&snap).unwrap();
    let back: Snapshot = serde_json::from_str(&j).unwrap();
    assert_eq!(back, snap);
}

// --- monitor ---------------------------------------------------------------

#[test]
fn monitor_diffs_snapshots() {
    let mut m = Monitor::new();
    let a = sample();
    assert!(m.update(&a).is_empty(), "first snapshot only primes");
    assert_eq!(m.len(), 4, "listeners are left out by default");

    let mut b = a.clone();
    b.taken_at_ms = a.taken_at_ms + 2000;
    b.sockets.remove(0); // Safari's connection closed
    b.sockets[2].state = Some(TcpState::Established); // curl connected
    b.sockets.push(udp(21, "192.168.1.5:60000", Some("8.8.8.8:53")));
    let ev = m.update(&b);
    assert_eq!(ev.len(), 3, "{ev:#?}");
    assert!(ev.iter().any(|e| matches!(e, Event::Opened { socket, direction: Direction::Outgoing, .. } if socket.remote == Some(sa("8.8.8.8:53")))));
    assert!(ev.iter().any(|e| matches!(e, Event::StateChanged { from: Some(TcpState::SynSent), to: Some(TcpState::Established), .. })));
    assert!(ev.iter().any(|e| matches!(e, Event::Closed { process, open_for_ms: 2000, .. } if process.name == "Safari")));

    let mut m2 = Monitor::with_listeners(true);
    m2.update(&b);
    assert_eq!(m2.len(), 6);
}

// --- rules -----------------------------------------------------------------

fn q<'a>(p: &'a ProcessInfo, remote: &str, host: Option<&'a str>) -> Query<'a> {
    let r = sa(remote);
    Query { process: p, direction: Direction::Outgoing, protocol: Protocol::Tcp, remote_ip: Some(r.ip()), remote_host: host, remote_port: Some(r.port()) }
}

#[test]
fn parse_remotes_and_ports() {
    assert_eq!(RemoteMatch::parse("any").unwrap(), RemoteMatch::Any);
    assert_eq!(RemoteMatch::parse("local-net").unwrap(), RemoteMatch::LocalNetwork);
    assert_eq!(RemoteMatch::parse("1.2.3.4").unwrap(), RemoteMatch::Address("1.2.3.4".parse().unwrap()));
    assert_eq!(RemoteMatch::parse("[::ffff:1.2.3.4]").unwrap(), RemoteMatch::Address("1.2.3.4".parse().unwrap()));
    assert_eq!(RemoteMatch::parse("10.0.0.0/8").unwrap(), RemoteMatch::Network { addr: "10.0.0.0".parse().unwrap(), prefix: 8 });
    assert!(RemoteMatch::parse("10.0.0.0/33").is_err());
    assert_eq!(RemoteMatch::parse("*.Example.com").unwrap(), RemoteMatch::Domain("example.com".into()));
    assert_eq!(RemoteMatch::parse(".example.com").unwrap(), RemoteMatch::Domain("example.com".into()));
    assert_eq!(RemoteMatch::parse("api.example.com.").unwrap(), RemoteMatch::Host("api.example.com".into()));
    assert!(RemoteMatch::parse("not a host").is_err());

    let p = Ports::parse("80, 443,8000-8100").unwrap();
    assert!(p.matches(Some(443)) && p.matches(Some(8050)) && !p.matches(Some(22)) && !p.matches(None));
    assert_eq!(p.label(), "80, 443, 8000-8100");
    assert!(Ports::parse("any").unwrap().matches(None));
    assert!(Ports::parse("90-80").is_err());
    assert!(Ports::parse("70000").is_err());
}

#[test]
fn networks() {
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();
    assert!(in_network(ip("10.2.3.4"), ip("10.0.0.0"), 8));
    assert!(!in_network(ip("11.2.3.4"), ip("10.0.0.0"), 8));
    assert!(in_network(ip("::ffff:10.2.3.4"), ip("10.0.0.0"), 8));
    assert!(in_network(ip("1.2.3.4"), ip("0.0.0.0"), 0));
    assert!(in_network(ip("2001:db8::5"), ip("2001:db8::"), 32));
    assert!(!in_network(ip("2001:db9::5"), ip("2001:db8::"), 32));
    assert!(!in_network(ip("10.0.0.1"), ip("2001:db8::"), 0));
}

#[test]
fn app_matching() {
    let helper = proc_(1, CHROME_HELPER);
    assert!(AppMatch::Bundle("/Applications/Google Chrome.app".into()).matches(&helper));
    assert!(!AppMatch::Bundle("/Applications/Google".into()).matches(&helper), "paths match by component");
    assert!(!AppMatch::Bundle("/Applications/Safari.app".into()).matches(&helper));
    assert!(AppMatch::Path(CURL.into()).matches(&proc_(2, CURL)));
    assert!(AppMatch::Name("CURL".into()).matches(&proc_(2, CURL)));
    assert!(AppMatch::Name("google chrome".into()).matches(&helper), "app name covers helpers");
    assert!(!AppMatch::Name("Safari".into()).matches(&helper));
    assert_eq!(AppMatch::for_path(Path::new("/Applications/Safari.app/")), AppMatch::Bundle("/Applications/Safari.app".into()));
    assert_eq!(AppMatch::for_path(Path::new(SAFARI)), AppMatch::Bundle("/Applications/Safari.app".into()));
    assert_eq!(AppMatch::for_path(Path::new(CURL)), AppMatch::Path(CURL.into()));
}

#[test]
fn most_specific_rule_wins() {
    let safari = proc_(1, SAFARI);
    let curl = proc_(2, CURL);
    let mut set = RuleSet::default();
    // Radio Silence: block Safari entirely…
    let block = set.add(Rule::for_app(AppMatch::Bundle("/Applications/Safari.app".into()), Action::Deny));
    // …except Apple's servers on 443.
    let apple = set.add(Rule { action: Action::Allow, app: AppMatch::Bundle("/Applications/Safari.app".into()), remote: RemoteMatch::parse("17.0.0.0/8").unwrap(), ports: Ports::parse("443").unwrap(), ..Rule::default() });
    // Any app may talk to the local network.
    let lan = set.add(Rule { action: Action::Allow, remote: RemoteMatch::LocalNetwork, ..Rule::default() });

    assert_eq!(set.evaluate(&q(&safari, "17.253.144.10:443", None)), Verdict { action: Action::Allow, rule: Some(apple.clone()) });
    assert_eq!(set.evaluate(&q(&safari, "17.253.144.10:80", None)).rule, Some(block.clone()));
    assert_eq!(set.evaluate(&q(&safari, "8.8.8.8:443", None)).action, Action::Deny);
    // Safari's own deny is more specific (named app) than the any-app LAN allow.
    assert_eq!(set.evaluate(&q(&safari, "192.168.1.1:80", None)).rule, Some(block));
    assert_eq!(set.evaluate(&q(&curl, "192.168.1.1:80", None)).rule, Some(lan));
    assert_eq!(set.evaluate(&q(&curl, "8.8.8.8:443", None)), Verdict { action: Action::Allow, rule: None });

    set.default_action = Action::Deny;
    assert_eq!(set.evaluate(&q(&curl, "8.8.8.8:443", None)), Verdict { action: Action::Deny, rule: None });
}

#[test]
fn ties_deny_and_priority() {
    let curl = proc_(2, CURL);
    let mut set = RuleSet::default();
    set.add(Rule { action: Action::Allow, remote: RemoteMatch::Domain("example.com".into()), ..Rule::default() });
    let deny = set.add(Rule { action: Action::Deny, remote: RemoteMatch::Domain("example.com".into()), ..Rule::default() });
    assert_eq!(set.evaluate(&q(&curl, "93.184.215.14:443", Some("www.example.com"))).rule, Some(deny.clone()));
    // Domain rules need a host name.
    assert_eq!(set.evaluate(&q(&curl, "93.184.215.14:443", None)).rule, None);
    assert_eq!(set.evaluate(&q(&curl, "1.1.1.1:443", Some("notexample.com"))).rule, None);
    // High priority beats a more specific regular rule.
    let hp = set.add(Rule { action: Action::Allow, high_priority: true, ..Rule::default() });
    assert_eq!(set.evaluate(&q(&curl, "93.184.215.14:443", Some("example.com"))).rule, Some(hp.clone()));
    // Disabled rules don't count.
    set.get_mut(&hp).unwrap().enabled = false;
    assert_eq!(set.evaluate(&q(&curl, "93.184.215.14:443", Some("example.com"))).rule, Some(deny));
}

#[test]
fn direction_and_protocol() {
    let curl = proc_(2, CURL);
    let mut set = RuleSet::default();
    let inc = set.add(Rule { action: Action::Deny, direction: DirectionMatch::Incoming, ..Rule::default() });
    let udp_rule = set.add(Rule { action: Action::Ask, protocol: Some(Protocol::Udp), direction: DirectionMatch::Both, ..Rule::default() });
    let mut query = q(&curl, "8.8.8.8:53", None);
    assert_eq!(set.evaluate(&query).rule, None);
    query.protocol = Protocol::Udp;
    assert_eq!(set.evaluate(&query).rule, Some(udp_rule));
    query.protocol = Protocol::Tcp;
    query.direction = Direction::Incoming;
    assert_eq!(set.evaluate(&query).rule, Some(inc));
}

#[test]
fn ids_storage_and_json_shape() {
    let dir = std::env::temp_dir().join(format!("net-monitor-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(RuleSet::load(&dir).unwrap(), RuleSet::default());

    let mut set = RuleSet::default();
    let a = set.add(Rule::for_app(AppMatch::Path(CURL.into()), Action::Deny));
    let b = set.add(Rule { id: a.clone(), ..Rule::default() });
    assert_eq!((a.as_str(), b.as_str()), ("r1", "r2"));
    assert!(set.rules.iter().all(|r| r.created > 0));
    set.save(&dir).unwrap();
    assert_eq!(RuleSet::load(&dir).unwrap(), set);
    assert!(set.remove("r1").is_some());
    assert!(set.remove("r1").is_none());
    assert_eq!(set.add(Rule::default()), "r3");

    let j = serde_json::to_value(&set.rules[0]).unwrap();
    assert_eq!(j["app"], serde_json::json!({"type": "any"}));
    assert_eq!(j["remote"], serde_json::json!({"type": "any"}));
    assert!(j.get("ports").is_none() && j.get("highPriority").is_none());
    // A hand-written minimal rule fills in the defaults.
    let r: Rule = serde_json::from_str(r#"{"id":"x","app":{"type":"bundle","value":"/Applications/Safari.app"},"remote":{"type":"domain","value":"example.com"},"ports":[{"start":443,"end":443}]}"#).unwrap();
    assert!(r.enabled && r.action == Action::Deny && r.direction == DirectionMatch::Outgoing);
    assert_eq!(r.summary(), "deny Safari → *.example.com, port 443");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn audit_snapshot() {
    let mut set = RuleSet::default();
    set.add(Rule::for_app(AppMatch::Bundle("/Applications/Google Chrome.app".into()), Action::Deny));
    let audit = set.audit(&sample());
    assert_eq!(audit.len(), 4, "only connections with a remote end");
    let chrome: Vec<_> = audit.iter().filter(|e| e.app == "Google Chrome").collect();
    assert_eq!(chrome.len(), 1);
    assert_eq!(chrome[0].verdict.action, Action::Deny);
    // sshd's accepted connection is incoming; the outgoing-only rule set allows it by default.
    assert!(audit.iter().filter(|e| e.app != "Google Chrome").all(|e| e.verdict == Verdict { action: Action::Allow, rule: None }));
}

#[test]
fn lsrules_import() {
    let json = r#"{
        "name": "Example group",
        "description": "test",
        "rules": [
            {"process": "/Applications/Safari.app/Contents/MacOS/Safari", "remote-domains": "tracker.example, ads.example", "action": "deny", "ports": "80, 443", "protocol": "tcp"},
            {"process": "any", "remote": "local-net", "action": "allow", "direction": "incoming", "priority": "high"},
            {"process": "/usr/bin/curl", "remote-addresses": ["10.0.0.0/8", "1.1.1.1", "1.0.0.1-1.0.0.9"], "action": "ask", "protocol": 17, "disabled": true, "notes": "n"},
            {"process": "/usr/bin/nc", "remote": "bonjour", "action": "allow"},
            {"process": "/usr/bin/ssh", "action": "allow"},
            {"process": "/usr/bin/x", "action": "explode"}
        ],
        "denied-remote-hosts": ["evil.example", "6.6.6.6"],
        "denied-remote-domains": "bad.example"
    }"#;
    let imp = import_lsrules(json).unwrap();
    assert_eq!(imp.name, "Example group");
    let summaries: Vec<String> = imp.rules.iter().map(|r| r.summary()).collect();
    assert_eq!(
        summaries,
        vec![
            "deny Safari → *.tracker.example, port 80, 443 (TCP)",
            "deny Safari → *.ads.example, port 80, 443 (TCP)",
            "allow any app ← local network",
            "ask /usr/bin/curl → 10.0.0.0/8 (UDP) [off]",
            "ask /usr/bin/curl → 1.1.1.1 (UDP) [off]",
            "allow /usr/bin/ssh → any server",
            "deny any app → evil.example",
            "deny any app → 6.6.6.6",
            "deny any app → *.bad.example",
        ],
        "{summaries:#?}"
    );
    assert!(imp.rules[2].high_priority);
    assert_eq!(imp.rules[3].note, "n");
    assert!(imp.rules.iter().all(|r| r.source.as_deref() == Some("Example group")));
    assert_eq!(imp.skipped.len(), 4, "{:#?}", imp.skipped);
    assert!(imp.skipped.iter().any(|s| s.contains("1.0.0.1-1.0.0.9")));
    assert!(imp.skipped.iter().any(|s| s.contains("bonjour")));
    assert!(imp.skipped.iter().any(|s| s.contains("not imported")));
    assert!(imp.skipped.iter().any(|s| s.contains("explode")));

    assert!(import_lsrules("[]").is_err());
    assert!(import_lsrules("nope").is_err());
}

// --- macOS backend -----------------------------------------------------------

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::platform_test_hooks::*;

    fn put_i32(b: &mut [u8], off: usize, v: i32) {
        b[off..off + 4].copy_from_slice(&v.to_ne_bytes());
    }

    #[allow(clippy::too_many_arguments)]
    fn fdinfo(family: i32, kind: i32, proto: i32, vflag: u8, laddr: [u8; 16], lport: u16, faddr: [u8; 16], fport: u16, state: i32) -> Vec<u8> {
        let mut b = vec![0u8; SOCKET_FDINFO_SIZE];
        put_i32(&mut b, OFF_SOI_FAMILY, family);
        put_i32(&mut b, OFF_SOI_KIND, kind);
        put_i32(&mut b, OFF_SOI_PROTOCOL, proto);
        b[OFF_VFLAG] = vflag;
        b[OFF_LADDR..OFF_LADDR + 16].copy_from_slice(&laddr);
        b[OFF_FADDR..OFF_FADDR + 16].copy_from_slice(&faddr);
        put_i32(&mut b, OFF_LPORT, lport.to_be() as i32);
        put_i32(&mut b, OFF_FPORT, fport.to_be() as i32);
        put_i32(&mut b, OFF_TCP_STATE, state);
        b[OFF_SOI_SO..OFF_SOI_SO + 8].copy_from_slice(&7u64.to_ne_bytes());
        b
    }

    fn in4(a: [u8; 4]) -> [u8; 16] {
        let mut o = [0u8; 16];
        o[12..].copy_from_slice(&a);
        o
    }

    #[test]
    fn parses_socket_fdinfo() {
        let b = fdinfo(libc::AF_INET, SOCKINFO_TCP, libc::IPPROTO_TCP, 1, in4([192, 168, 1, 5]), 50000, in4([17, 253, 144, 10]), 443, 4);
        let (so, s) = parse_socket_fdinfo(9, &b).unwrap();
        assert_eq!(so, 7);
        assert_eq!(s, tcp(9, "192.168.1.5:50000", Some("17.253.144.10:443"), TcpState::Established));

        let b = fdinfo(libc::AF_INET6, SOCKINFO_TCP, libc::IPPROTO_TCP, 2, [0; 16], 22, [0; 16], 0, 1);
        assert_eq!(parse_socket_fdinfo(9, &b).unwrap().1, tcp(9, "[::]:22", None, TcpState::Listen));

        let mut v6 = [0u8; 16];
        v6[15] = 1;
        let b = fdinfo(libc::AF_INET6, SOCKINFO_IN, libc::IPPROTO_UDP, 2, v6, 5353, [0; 16], 0, 0);
        assert_eq!(parse_socket_fdinfo(9, &b).unwrap().1, udp(9, "[::1]:5353", None));

        // Dual-stack socket talking IPv4.
        let b = fdinfo(libc::AF_INET6, SOCKINFO_TCP, libc::IPPROTO_TCP, 1, in4([10, 0, 0, 2]), 1, in4([1, 1, 1, 1]), 853, 4);
        assert_eq!(parse_socket_fdinfo(9, &b).unwrap().1.remote, Some(sa("1.1.1.1:853")));

        // Unix sockets and short buffers are ignored.
        assert!(parse_socket_fdinfo(9, &fdinfo(libc::AF_UNIX, 3, 0, 0, [0; 16], 0, [0; 16], 0, 0)).is_none());
        assert!(parse_socket_fdinfo(9, &b[..100]).is_none());
    }

    /// The real thing: this test process's own listening socket shows up.
    #[test]
    fn live_snapshot_sees_own_socket() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let _c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let snap = crate::snapshot().unwrap();
        let me = std::process::id();
        let mine: Vec<&Socket> = snap.sockets.iter().filter(|s| s.pid == me).collect();
        assert!(mine.iter().any(|s| s.local.port() == port && s.state == Some(TcpState::Listen)), "{mine:#?}");
        assert!(mine.iter().any(|s| s.remote.map(|r| r.port()) == Some(port) && s.state == Some(TcpState::Established)), "{mine:#?}");
        let p = snap.process(me).unwrap();
        assert!(p.path.is_some());
        // The accepted side counts as incoming once accepted (it shares the listener's port).
        let (_srv, _) = l.accept().unwrap();
        let snap = crate::snapshot().unwrap();
        let dirs = snap.directions();
        let incoming = snap.sockets.iter().zip(dirs).any(|(s, d)| s.pid == me && s.local.port() == port && s.remote.is_some() && d == Direction::Incoming);
        assert!(incoming);
    }
}
