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
    }
    std::thread::Builder::new()
        .name("mwb-events".into())
        .spawn(move || {
            let mut cb = None;
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
