//! Mouse Without Borders glue around a running engine: one thread takes the
//! engine's events and fans them out (clipboard, persisting what discovery
//! found, logging) and keeps `mwb-status.json` up to date for the settings
//! window.

use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, UNIX_EPOCH};

use mwb::{EngineEvent, Matrix};
use pm_config::{AppConfig, MwbMachine, MwbStatus, MWB_STATUS_REFRESH_SECS};

use crate::clipboard;

fn config_path() -> Option<PathBuf> {
    crate::CONFIG_PATH.get().cloned().or_else(AppConfig::default_path)
}

/// Start the event thread for a freshly started engine. It ends when that
/// engine stops.
pub fn start(h: &mwb::EngineHandle) {
    clipboard::start_watch_once();
    let Some(rx) = h.events() else { return };
    let mon = h.monitor();
    let path = config_path();
    if let (Some(p), Some(st)) = (&path, mon.status()) {
        write_status(p, Some(&st));
        write_diagnostics(p, &st);
    }
    std::thread::Builder::new()
        .name("mwb-events".into())
        .spawn(move || {
            let mut cb = None;
            let mut last_diag = std::time::Instant::now();
            loop {
                let ev = match rx.recv_timeout(Duration::from_secs(MWB_STATUS_REFRESH_SECS)) {
                    Ok(ev) => Some(ev),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                if let Some(ev) = &ev {
                    if !clipboard::on_event(&mut cb, ev) {
                        log::info!("mwb: {ev:?}");
                    }
                    if let Some(p) = &path {
                        persist(p, ev);
                    }
                }
                // A stopped engine leaves the file to its successor (or to `apply`).
                let Some(st) = mon.status() else { continue };
                if let Some(p) = &path {
                    if ev.as_ref().is_none_or(affects_status) {
                        write_status(p, Some(&st));
                    }
                    if last_diag.elapsed() >= Duration::from_secs(MWB_STATUS_REFRESH_SECS) {
                        last_diag = std::time::Instant::now();
                        write_diagnostics(p, &st);
                    }
                }
            }
        })
        .expect("spawn mwb-events");
}

fn affects_status(ev: &EngineEvent) -> bool {
    !matches!(ev, EngineEvent::ClipboardText { .. } | EngineEvent::ActiveMachineChanged { .. })
}

/// Save what the engine learnt into `config.json`: a discovered machine's
/// address, and the layout when discovery or a peer changed it. The config
/// watcher sees the write and applies it to the running engine without a
/// restart (and it changes nothing there, so there is no loop).
fn persist(path: &Path, ev: &EngineEvent) {
    let r = match ev {
        EngineEvent::PeerDiscovered { name, addr, .. } => persist_peer(path, name, *addr),
        EngineEvent::MatrixChanged(m) => persist_matrix(path, m),
        _ => Ok(()),
    };
    if let Err(e) = r {
        log::warn!("mouse without borders: saving to the config: {e:#}");
    }
}

/// Write `peers[name] = address` unless the config already says so.
pub(crate) fn persist_peer(path: &Path, name: &str, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let cur = AppConfig::load_or_default(path)?;
    let spec = mwb::peer_spec(addr, cur.mwb.settings.port);
    if peer_entry(&cur, name).is_some_and(|(_, v)| v.trim() == spec) {
        return Ok(());
    }
    log::info!("mouse without borders: saving the address of {name}: {spec}");
    pm_config::edit_at(path, |c| {
        let peers = &mut c.mwb.settings.peers;
        peers.retain(|k, _| !k.eq_ignore_ascii_case(name));
        peers.insert(name.to_string(), spec);
    })
}

fn peer_entry<'a>(c: &'a AppConfig, name: &str) -> Option<(&'a String, &'a String)> {
    c.mwb.settings.peers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name))
}

/// Slots as stored in `config.json`: trimmed, trailing empty slots dropped.
fn matrix_vec(m: &Matrix) -> Vec<String> {
    let mut v: Vec<String> = m.slots.iter().map(|s| s.trim().to_string()).collect();
    while v.last().is_some_and(|s| s.is_empty()) {
        v.pop();
    }
    v
}

/// Write the layout unless the config already has it.
pub(crate) fn persist_matrix(path: &Path, m: &Matrix) -> anyhow::Result<()> {
    let cur = AppConfig::load_or_default(path)?;
    let s = &cur.mwb.settings;
    if s.matrix() == *m {
        return Ok(());
    }
    log::info!("mouse without borders: saving the layout {:?}", matrix_vec(m));
    pm_config::edit_at(path, |c| {
        let s = &mut c.mwb.settings;
        s.matrix = matrix_vec(m);
        s.one_row = m.one_row;
        s.wrap_around = m.circle;
    })
}

/// The status file contents for `st` (`None`: not running).
pub(crate) fn status_of(st: Option<&mwb::Status>, port: u16) -> MwbStatus {
    let Some(st) = st else { return MwbStatus::default().stamp() };
    let mut machines: Vec<MwbMachine> = Vec::new();
    for p in &st.peers {
        let found = st.discovered.iter().find(|d| d.name.eq_ignore_ascii_case(&p.name));
        let address = match (found, p.ip) {
            (Some(d), _) => mwb::peer_spec(d.addr, port),
            (None, Some(ip)) => ip.to_string(),
            (None, None) => String::new(),
        };
        machines.push(MwbMachine { name: p.name.clone(), address, connected: p.connections > 0, discovered: found.is_some() });
    }
    for d in &st.discovered {
        if !machines.iter().any(|m| m.name.eq_ignore_ascii_case(&d.name)) {
            machines.push(MwbMachine { name: d.name.clone(), address: mwb::peer_spec(d.addr, port), connected: false, discovered: true });
        }
    }
    machines.sort_by_key(|m| m.name.to_ascii_lowercase());
    MwbStatus {
        at: 0,
        running: true,
        machine_name: st.machine_name.clone(),
        searching: st.searching,
        auto_discover: st.auto_discover,
        last_search: st.last_search.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64),
        machines,
        other_key: st.other_key.iter().map(|a| mwb::peer_spec(*a, port)).collect(),
    }
    .stamp()
}

/// Rewrite `mwb-status.json` next to the config at `path`.
pub(crate) fn write_status(path: &Path, st: Option<&mwb::Status>) {
    let port = AppConfig::load_or_default(path).map(|c| c.mwb.settings.port).unwrap_or(mwb::DEFAULT_PORT);
    if let Err(e) = status_of(st, port).save_for(path) {
        log::debug!("mwb status file: {e:#}");
    }
}

/// Written next to `config.json` by the running app for `powermac mwb doctor`:
/// recent incoming connections, learnt addresses and the LLMNR responder.
pub const DIAG_FILE: &str = "mwb-diagnostics.json";

fn ms(t: std::time::SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The diagnostics file contents for `st`.
pub(crate) fn diagnostics_of(st: &mwb::Status) -> serde_json::Value {
    let llmnr = st.llmnr.as_ref().map(|l| {
        serde_json::json!({
            "running": l.running,
            "name": l.name,
            "port": l.port,
            "ipv4": l.ipv4,
            "ipv6": l.ipv6,
            "groupsV4": l.groups_v4.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
            "queries": l.queries,
            "answered": l.answered,
            "lastAnswer": l.last_answer.as_ref().map(|(from, name, ip, at)| serde_json::json!({
                "from": from.to_string(), "name": name, "answer": ip.map(|i| i.to_string()), "at": ms(*at),
            })),
            "lastOther": l.last_other.as_ref().map(|(from, name)| serde_json::json!({ "from": from.to_string(), "name": name })),
            "error": l.error,
        })
    });
    serde_json::json!({
        "at": ms(std::time::SystemTime::now()),
        "machineName": st.machine_name,
        "incoming": st.incoming.iter().map(|a| serde_json::json!({
            "from": a.from.to_string(), "at": ms(a.at), "result": a.result,
        })).collect::<Vec<_>>(),
        "learned": st.learned.iter().map(|(n, a)| serde_json::json!({ "name": n, "addr": a.to_string() })).collect::<Vec<_>>(),
        "peers": st.peers.iter().map(|p| serde_json::json!({
            "name": p.name, "connections": p.connections, "ip": p.ip.map(|i| i.to_string()),
        })).collect::<Vec<_>>(),
        "llmnr": llmnr,
    })
}

fn write_diagnostics(path: &Path, st: &mwb::Status) {
    let p = path.with_file_name(DIAG_FILE);
    let tmp = p.with_extension("json.tmp");
    let r = serde_json::to_string_pretty(&diagnostics_of(st))
        .map_err(anyhow::Error::from)
        .and_then(|s| Ok(std::fs::write(&tmp, s)?))
        .and_then(|_| Ok(std::fs::rename(&tmp, &p)?));
    if let Err(e) = r {
        log::debug!("mwb diagnostics file: {e:#}");
    }
}

/// Mark Mouse Without Borders as not running in the status file.
pub fn write_stopped() {
    if let Some(p) = config_path() {
        write_status(&p, None);
    }
}

/// Text of the tray's Mouse Without Borders status line.
pub fn status_line(st: &mwb::Status) -> String {
    let found = |n: &str| st.discovered.iter().any(|d| d.name.eq_ignore_ascii_case(n));
    let peers: Vec<String> = st
        .peers
        .iter()
        .filter(|p| p.connections > 0)
        .map(|p| if found(&p.name) { format!("{} (found automatically)", p.name) } else { p.name.clone() })
        .collect();
    let waiting: Vec<String> = st
        .discovered
        .iter()
        .filter(|d| !st.peers.iter().any(|p| p.name.eq_ignore_ascii_case(&d.name)))
        .map(|d| format!("{} ({})", d.name, d.addr.ip()))
        .collect();
    let mut text = if let Some(by) = &st.controlled_by {
        format!("Controlled by {by}")
    } else if !peers.is_empty() {
        format!("Connected: {}", peers.join(", "))
    } else if !waiting.is_empty() {
        format!("Found {}, reconnecting", waiting.join(", "))
    } else if st.searching {
        "Mouse Without Borders: searching…".to_string()
    } else {
        "Mouse Without Borders: no peers".to_string()
    };
    if st.searching && !text.contains("searching") {
        text.push_str(" · searching…");
    }
    text
}

// ----------------------------------------------------------------------
// `powermac mwb doctor`
// ----------------------------------------------------------------------

/// Run a command and return its stdout (empty when it can't run).
fn cmd_out(prog: &str, args: &[&str]) -> String {
    std::process::Command::new(prog)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// The interface the route to `dest` ("default" or an IP) uses (macOS `route -n get`).
fn route_interface(dest: &str) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    cmd_out("route", &["-n", "get", dest])
        .lines()
        .find_map(|l| l.trim().strip_prefix("interface:").map(|s| s.trim().to_string()))
}

/// A VPN-ish interface (NordVPN, WireGuard, IKEv2...).
fn is_tunnel(ifname: &str) -> bool {
    ["utun", "ppp", "ipsec", "tun", "tap", "wg"].iter().any(|p| ifname.starts_with(p))
}

/// What listens on TCP `port` (process name), or whether the port is taken at all.
fn tcp_listener(port: u16) -> Option<String> {
    if cfg!(unix) {
        let out = cmd_out("lsof", &["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN"]);
        if let Some(l) = out.lines().nth(1) {
            let mut w = l.split_whitespace();
            return Some(format!("{} (pid {})", w.next().unwrap_or("?"), w.next().unwrap_or("?")));
        }
    }
    match std::net::TcpListener::bind(("0.0.0.0", port)) {
        Ok(_) => None,
        Err(_) => Some("another process".into()),
    }
}

fn udp_owner(port: u16) -> Option<String> {
    if !cfg!(unix) {
        return None;
    }
    let out = cmd_out("lsof", &["-nP", &format!("-iUDP:{port}")]);
    let owners: std::collections::BTreeSet<String> = out
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some(format!("{} (pid {})", w.next()?, w.next()?))
        })
        .collect();
    (!owners.is_empty()).then(|| owners.into_iter().collect::<Vec<_>>().join(", "))
}

/// The executable the macOS firewall has to let in: the process listening on
/// the MWB port (the running PowerMac), else this executable when it's in an
/// app bundle, else /Applications/PowerMac.app.
fn app_executable(msg_port: u16) -> Option<PathBuf> {
    let listener_pid = cmd_out("lsof", &["-nP", "-t", &format!("-iTCP:{msg_port}"), "-sTCP:LISTEN"]);
    if let Some(pid) = listener_pid.lines().next().filter(|p| !p.is_empty()) {
        let comm = cmd_out("ps", &["-o", "comm=", "-p", pid]);
        if comm.starts_with('/') {
            return Some(PathBuf::from(comm));
        }
    }
    let exe = std::env::current_exe().ok()?;
    if exe.to_string_lossy().contains(".app/Contents/MacOS/") {
        return Some(exe);
    }
    let p = PathBuf::from("/Applications/PowerMac.app");
    p.exists().then_some(p)
}

fn ago(at_ms: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let s = now.saturating_sub(at_ms) / 1000;
    match s {
        0..=89 => format!("{s} s ago"),
        90..=5399 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

/// Copy-pasteable PowerShell for the Windows side.
pub(crate) fn powershell_steps(name: &str, ip: &str, port: u16, key_hint: &str) -> String {
    pm_config::mwb_help::powershell_steps(name, ip, port, key_hint)
}

/// `powermac mwb doctor`: everything that decides whether a Windows PC can pair with this Mac.
pub fn doctor(path: &Path) -> anyhow::Result<()> {
    let cfg = AppConfig::load_or_default(path)?;
    let s = &cfg.mwb.settings;
    let name = s.effective_machine_name();
    let ifs = mwb::llmnr::local_ifs();
    let primary = mwb::llmnr::primary_ipv4(&ifs);
    let msg_port = s.port.wrapping_add(1);
    let mut problems: Vec<String> = Vec::new();

    println!("== This Mac");
    println!("Mouse Without Borders enabled: {}", cfg.mwb.enabled);
    println!("machine name: {name}   (type {} in PowerToys; case doesn't matter)", name.to_uppercase());
    if name.len() > 15 {
        println!("  note: longer than 15 characters; fine for PowerToys and LLMNR, NetBIOS would see only {:?}", &name[..15]);
    }
    if cfg!(target_os = "macos") {
        let local = cmd_out("scutil", &["--get", "LocalHostName"]);
        if !local.is_empty() {
            println!("Bonjour name: {local}.local{}", if local.eq_ignore_ascii_case(&name) { "" } else { "   (differs from the MWB name)" });
        }
    }
    for i in &ifs {
        if i.ip.is_loopback() {
            continue;
        }
        let tag = if Some(i.ip) == primary { "  <- LAN address Windows should use" } else if i.p2p { "  (tunnel / VPN)" } else { "" };
        println!("IPv4: {}/{}{tag}", i.ip, u32::from(i.netmask).leading_ones());
    }
    if primary.is_none() {
        problems.push("no LAN IPv4 address".into());
    }
    match tcp_listener(msg_port) {
        Some(who) => println!("TCP {msg_port} (MWB messages): listening - {who}"),
        None => {
            println!("TCP {msg_port} (MWB messages): NOT listening");
            problems.push(format!("nothing listens on TCP {msg_port}: is PowerMac running with Mouse Without Borders on?"));
        }
    }
    println!("TCP {} (clipboard/file socket): {}", s.port, tcp_listener(s.port).unwrap_or_else(|| "not used by PowerMac (not needed to pair)".into()));

    println!("\n== Security key");
    let key = mwb::normalize_key(&s.key);
    let valid = mwb::crypto::is_key_valid(&key);
    println!("valid: {valid} ({} characters after removing whitespace; PowerToys needs >= 16)", key.chars().count());
    if !valid {
        problems.push("the security key is too short".into());
    } else {
        let magic = mwb::get_24bit_hash(&key);
        if magic & 0x8000_0000 != 0 {
            println!("bit-31 quirk: AFFECTED (magic {magic:#010x}): Windows MWB rejects every package with this key; generate a new key");
            problems.push("the key trips the bit-31 magic-number quirk; generate a new one".into());
        } else {
            println!("bit-31 quirk: not affected (magic {magic:#010x})");
        }
    }

    println!("\n== Name lookups from Windows (LLMNR, UDP {})", s.llmnr_port);
    println!("answerNameQueries: {}", s.answer_name_queries);
    match udp_owner(s.llmnr_port) {
        Some(who) => println!("UDP {}: bound by {who}", s.llmnr_port),
        None => println!("UDP {}: not bound", s.llmnr_port),
    }
    let timeout = Duration::from_millis(1500);
    let mut targets: Vec<std::net::SocketAddr> = vec![(std::net::Ipv4Addr::LOCALHOST, s.llmnr_port).into()];
    if let Some(p) = primary {
        targets.push((p, s.llmnr_port).into());
    }
    for t in targets {
        match mwb::llmnr::query_a(&name.to_uppercase(), t, timeout) {
            Ok(Some(a)) => println!("query {} at {t}: answered {:?}", name.to_uppercase(), a),
            Ok(None) => {
                println!("query {} at {t}: no answer", name.to_uppercase());
                if s.answer_name_queries && t.ip().is_loopback() {
                    problems.push("the LLMNR responder doesn't answer (old PowerMac build running, or UDP 5355 taken)".into());
                }
            }
            Err(e) => println!("query at {t}: {e}"),
        }
    }
    println!("(NetBIOS name service, UDP 137, needs root and isn't offered; LLMNR is what Windows asks first after DNS.)");

    println!("\n== VPN / routing");
    match route_interface("default") {
        Some(i) if is_tunnel(&i) => {
            println!("default route: {i}  <- a VPN tunnel (NordVPN and similar)");
            problems.push(format!(
                "the default route goes through {i} (VPN): the PC's connections and name queries may not reach this Mac; quit the VPN or allow LAN traffic in it"
            ));
        }
        Some(i) => println!("default route: {i}"),
        None => println!("default route: unknown"),
    }
    let tunnels: Vec<String> = ifs.iter().filter(|i| i.p2p && !i.ip.is_loopback()).map(|i| i.ip.to_string()).collect();
    if !tunnels.is_empty() {
        println!("tunnel addresses: {}", tunnels.join(", "));
    }

    let diag: Option<serde_json::Value> =
        std::fs::read_to_string(path.with_file_name(DIAG_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok());
    let status = MwbStatus::load_for(path).filter(|st| st.is_fresh());
    let mut peer_ips: Vec<String> = s.peers.values().map(|v| v.split(':').next().unwrap_or("").to_string()).collect();
    if let Some(st) = &status {
        peer_ips.extend(st.machines.iter().map(|m| m.address.split(':').next().unwrap_or("").to_string()));
    }
    peer_ips.retain(|p| p.parse::<std::net::Ipv4Addr>().is_ok());
    peer_ips.sort();
    peer_ips.dedup();
    for ip in &peer_ips {
        if let Some(i) = route_interface(ip) {
            let warn = if is_tunnel(&i) { "  <- through a VPN tunnel, not the LAN" } else { "" };
            println!("route to peer {ip}: {i}{warn}");
            if is_tunnel(&i) {
                problems.push(format!("traffic to {ip} goes through {i}"));
            }
        }
    }

    println!("\n== macOS firewall");
    if cfg!(target_os = "macos") {
        let fw = "/usr/libexec/ApplicationFirewall/socketfilterfw";
        let state = cmd_out(fw, &["--getglobalstate"]);
        println!("{}", if state.is_empty() { "unknown".to_string() } else { state.clone() });
        let block_all = cmd_out(fw, &["--getblockall"]);
        if !block_all.is_empty() {
            println!("{block_all}");
            if block_all.contains("enabled") && !block_all.contains("disabled") {
                problems.push("the macOS firewall blocks all incoming connections".into());
            }
        }
        println!("{}", cmd_out(fw, &["--getstealthmode"]));
        if let Some(app) = app_executable(msg_port) {
            let r = cmd_out(fw, &["--getappblocked", &app.to_string_lossy()]);
            println!("{}: {}", app.display(), if r.is_empty() { "unknown" } else { &r });
            if r.contains("blocked") && !r.contains("not blocked") && !r.contains("permitted") {
                problems.push(format!("the macOS firewall blocks {}", app.display()));
            }
        }
    } else {
        println!("n/a");
    }

    println!("\n== Connections the running app has seen");
    match &status {
        Some(st) => {
            for m in &st.machines {
                println!("peer {}: {} {}", m.name, if m.connected { "connected" } else { "not connected" }, m.address);
            }
            if st.machines.is_empty() {
                println!("no peers");
            }
        }
        None => println!("no fresh status from a running PowerMac"),
    }
    let incoming = diag.as_ref().and_then(|d| d["incoming"].as_array().cloned()).unwrap_or_default();
    if !incoming.is_empty() {
        println!("last incoming connection attempts:");
        for a in incoming.iter().rev().take(10) {
            println!(
                "  {} from {}: {}",
                ago(a["at"].as_u64().unwrap_or(0)),
                a["from"].as_str().unwrap_or("?"),
                a["result"].as_str().unwrap_or("")
            );
        }
    } else if let Some(log) = crate::log_path().filter(|p| p.exists()) {
        // Older builds don't write the diagnostics file; their log has the same lines.
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().filter(|l| l.contains("incoming connection from")).collect();
        println!("last incoming connection attempts (from {}):", log.display());
        if lines.is_empty() {
            println!("  none: the PC never reached TCP {msg_port}");
        }
        for l in lines.iter().rev().take(10) {
            println!("  {l}");
        }
    } else {
        println!("no incoming connection attempts recorded: the PC never reached TCP {msg_port}");
    }
    if let Some(l) = diag.as_ref().map(|d| &d["llmnr"]).filter(|l| !l.is_null()) {
        println!(
            "LLMNR responder: {} queries, {} answered{}",
            l["queries"],
            l["answered"],
            l["lastAnswer"]["from"].as_str().map(|f| format!(", last for {} from {f}", l["lastAnswer"]["name"].as_str().unwrap_or(""))).unwrap_or_default()
        );
    }

    println!("\n== On the Windows PC (PowerShell)");
    let ip = primary.map(|i| i.to_string()).unwrap_or_else(|| "<this Mac's IP>".into());
    println!("{}", powershell_steps(&name, &ip, s.port, "(the same key as here: tray > Copy security key)"));

    println!("\n== Summary");
    if problems.is_empty() {
        println!("no problems found on this Mac");
    }
    for p in problems {
        println!("- {p}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mwb::{DiscoveredPeer, PeerStatus, Status};

    fn status() -> Status {
        Status {
            machine_name: "MAC".into(),
            machine_id: 1,
            active_machine: "MAC".into(),
            active_machine_id: 1,
            controlled_by: None,
            peers: vec![],
            matrix: Matrix::from_names(&["MAC"], true, false),
            capture_active: true,
            discovered: vec![],
            searching: false,
            auto_discover: true,
            last_search: None,
            other_key: vec![],
            incoming: vec![],
            learned: vec![],
            llmnr: None,
        }
    }

    #[test]
    fn tray_line() {
        let mut st = status();
        assert_eq!(status_line(&st), "Mouse Without Borders: no peers");
        st.searching = true;
        assert_eq!(status_line(&st), "Mouse Without Borders: searching…");
        st.discovered.push(DiscoveredPeer { name: "PC".into(), id: 7, addr: "192.168.1.20:15101".parse().unwrap() });
        assert_eq!(status_line(&st), "Found PC (192.168.1.20), reconnecting · searching…");
        st.searching = false;
        st.peers.push(PeerStatus { name: "PC".into(), id: 7, connections: 2, ip: Some("192.168.1.20".parse().unwrap()) });
        st.peers.push(PeerStatus { name: "LAPTOP".into(), id: 8, connections: 1, ip: None });
        assert_eq!(status_line(&st), "Connected: PC (found automatically), LAPTOP");
        st.controlled_by = Some("PC".into());
        assert_eq!(status_line(&st), "Controlled by PC");
    }

    #[test]
    fn status_file_contents() {
        let mut st = status();
        st.discovered.push(DiscoveredPeer { name: "PC".into(), id: 7, addr: "192.168.1.20:15101".parse().unwrap() });
        st.discovered.push(DiscoveredPeer { name: "Old".into(), id: 9, addr: "192.168.1.30:15201".parse().unwrap() });
        st.peers.push(PeerStatus { name: "PC".into(), id: 7, connections: 2, ip: Some("192.168.1.20".parse().unwrap()) });
        st.peers.push(PeerStatus { name: "Laptop".into(), id: 8, connections: 1, ip: Some("10.0.0.5".parse().unwrap()) });
        st.other_key.push("192.168.1.40:15101".parse().unwrap());
        let f = status_of(Some(&st), 15100);
        assert!(f.running && f.is_fresh());
        assert_eq!(
            f.machines,
            vec![
                MwbMachine { name: "Laptop".into(), address: "10.0.0.5".into(), connected: true, discovered: false },
                MwbMachine { name: "Old".into(), address: "192.168.1.30:15200".into(), connected: false, discovered: true },
                MwbMachine { name: "PC".into(), address: "192.168.1.20".into(), connected: true, discovered: true },
            ]
        );
        assert_eq!(f.other_key, vec!["192.168.1.40".to_string()]);
        assert!(!status_of(None, 15100).running);
    }

    #[test]
    fn powershell_steps_use_the_name_and_ip() {
        let t = powershell_steps("MacBook-Pro", "10.0.0.112", 15100, "K");
        assert!(t.contains("Test-NetConnection 10.0.0.112 -Port 15101"));
        assert!(t.contains("Resolve-DnsName MACBOOK-PRO -LlmnrOnly"));
        assert!(t.contains("#        MACBOOK-PRO 10.0.0.112\n"));
        assert!(t.contains("-LocalPort 15100,15101 -RemoteAddress 10.0.0.112"));
    }

    #[test]
    fn diagnostics_contents() {
        let mut st = status();
        st.incoming.push(mwb::IncomingAttempt {
            from: "10.0.0.35:50123".parse().unwrap(),
            at: UNIX_EPOCH + Duration::from_secs(1),
            result: "trusted as 'PC'".into(),
        });
        let d = diagnostics_of(&st);
        assert_eq!(d["incoming"][0]["from"], "10.0.0.35:50123");
        assert_eq!(d["incoming"][0]["at"], 1000);
        assert!(d["llmnr"].is_null());
    }

    #[test]
    fn persisting_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("powermac-mwb-glue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let mut c = AppConfig::default();
        c.mwb.settings.machine_name = "MAC".into();
        c.mwb.settings.matrix = vec!["MAC".into()];
        c.mwb.settings.peers.insert("pc".into(), "10.0.0.9".into());
        c.save(&path).unwrap();

        persist_peer(&path, "PC", "192.168.1.20:15101".parse().unwrap()).unwrap();
        let back = AppConfig::load_or_default(&path).unwrap();
        assert_eq!(back.mwb.settings.peers.len(), 1);
        assert_eq!(back.mwb.settings.peers["PC"], "192.168.1.20");
        let m1 = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        persist_peer(&path, "pc", "192.168.1.20:15101".parse().unwrap()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), m1, "unchanged address rewrote the file");

        let m = Matrix::from_names(&["MAC", "PC"], true, false);
        persist_matrix(&path, &m).unwrap();
        assert_eq!(AppConfig::load_or_default(&path).unwrap().mwb.settings.matrix, ["MAC", "PC"]);
        let m2 = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        persist_matrix(&path, &m).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), m2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
