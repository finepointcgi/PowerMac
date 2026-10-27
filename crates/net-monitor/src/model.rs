//! What a snapshot of the system's sockets looks like: processes, their
//! TCP/UDP sockets, and the per-app grouping the monitor shows.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Transport protocol of a socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Tcp => "TCP",
            Protocol::Udp => "UDP",
        }
    }
}

/// TCP connection state (the BSD / RFC 793 states; Windows' extra
/// `DELETE_TCB` is reported as `Closed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    CloseWait,
    FinWait1,
    Closing,
    LastAck,
    FinWait2,
    TimeWait,
}

impl TcpState {
    /// From xnu's `TSI_S_*` numbering (`sys/proc_info.h`).
    pub fn from_xnu(v: i32) -> Option<Self> {
        use TcpState::*;
        Some(match v {
            0 => Closed,
            1 => Listen,
            2 => SynSent,
            3 => SynReceived,
            4 => Established,
            5 => CloseWait,
            6 => FinWait1,
            7 => Closing,
            8 => LastAck,
            9 => FinWait2,
            10 => TimeWait,
            _ => return None,
        })
    }

    /// From Windows' `MIB_TCP_STATE_*` numbering.
    pub fn from_mib(v: u32) -> Option<Self> {
        use TcpState::*;
        Some(match v {
            1 | 12 => Closed,
            2 => Listen,
            3 => SynSent,
            4 => SynReceived,
            5 => Established,
            6 => FinWait1,
            7 => FinWait2,
            8 => CloseWait,
            9 => Closing,
            10 => LastAck,
            11 => TimeWait,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        use TcpState::*;
        match self {
            Closed => "CLOSED",
            Listen => "LISTEN",
            SynSent => "SYN_SENT",
            SynReceived => "SYN_RECEIVED",
            Established => "ESTABLISHED",
            CloseWait => "CLOSE_WAIT",
            FinWait1 => "FIN_WAIT_1",
            Closing => "CLOSING",
            LastAck => "LAST_ACK",
            FinWait2 => "FIN_WAIT_2",
            TimeWait => "TIME_WAIT",
        }
    }
}

/// Which way a connection goes, as far as the socket table can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    /// This Mac connected (or sends datagrams) to the remote end.
    Outgoing,
    /// Another machine connected to a port this app listens on, or the
    /// socket is a listener / unconnected UDP socket waiting for traffic.
    Incoming,
}

/// Where the remote end of a connection is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    /// 127.0.0.0/8, ::1: another process on this machine.
    Loopback,
    /// Private, link-local, CGNAT and unique-local addresses, multicast and
    /// broadcast: the local network.
    LocalNetwork,
    /// Everything else.
    Internet,
}

impl Scope {
    pub fn of(ip: IpAddr) -> Scope {
        let ip = canonical_ip(ip);
        if ip.is_loopback() {
            return Scope::Loopback;
        }
        let local = match ip {
            IpAddr::V4(v4) => {
                let o = v4.octets();
                v4.is_private()
                    || v4.is_link_local()
                    || v4.is_multicast()
                    || v4.is_broadcast()
                    || v4.is_unspecified()
                    || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
            }
            IpAddr::V6(v6) => {
                let s = v6.segments();
                v6.is_unspecified()
                    || v6.is_multicast()
                    || (s[0] & 0xffc0) == 0xfe80 // link-local
                    || (s[0] & 0xfe00) == 0xfc00 // unique local
            }
        };
        if local {
            Scope::LocalNetwork
        } else {
            Scope::Internet
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::Loopback => "loopback",
            Scope::LocalNetwork => "local network",
            Scope::Internet => "internet",
        }
    }
}

/// IPv4-mapped IPv6 addresses (`::ffff:1.2.3.4`, what dual-stack sockets
/// report) as plain IPv4.
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => ip,
        },
        v4 => v4,
    }
}

/// A process that owns at least one socket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    /// Executable name (`proc_name` / the image file name).
    pub name: String,
    /// Full executable path, when the OS tells us.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl ProcessInfo {
    /// The outermost `.app` bundle the executable lives in, so helpers
    /// (`Google Chrome Helper.app` inside `Google Chrome.app`) count as their app.
    pub fn bundle(&self) -> Option<PathBuf> {
        self.path.as_deref().and_then(outermost_bundle)
    }

    /// What the monitor groups by and what app rules match: the bundle,
    /// else the executable path, else the name.
    pub fn app_key(&self) -> String {
        if let Some(b) = self.bundle() {
            return b.to_string_lossy().into_owned();
        }
        match &self.path {
            Some(p) => p.to_string_lossy().into_owned(),
            None => self.name.clone(),
        }
    }

    /// The name shown for the app: the bundle's name without `.app`, else
    /// the executable name.
    pub fn display_name(&self) -> String {
        if let Some(b) = self.bundle() {
            if let Some(stem) = b.file_stem() {
                return stem.to_string_lossy().into_owned();
            }
        }
        self.name.clone()
    }
}

/// The outermost `*.app` directory among `path`'s ancestors.
pub fn outermost_bundle(path: &Path) -> Option<PathBuf> {
    let mut acc = PathBuf::new();
    for c in path.components() {
        acc.push(c);
        let is_app = c.as_os_str().to_str().is_some_and(|s| s.len() > 4 && s.to_ascii_lowercase().ends_with(".app"));
        if is_app && acc != path {
            return Some(acc);
        }
    }
    None
}

/// One TCP or UDP socket owned by a process.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Socket {
    pub pid: u32,
    pub protocol: Protocol,
    pub local: SocketAddr,
    /// `None` for listeners and unconnected UDP sockets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<SocketAddr>,
    /// TCP only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TcpState>,
}

impl Socket {
    /// A TCP listener or a UDP socket with no peer: it waits for traffic.
    pub fn is_listener(&self) -> bool {
        match self.protocol {
            Protocol::Tcp => self.state == Some(TcpState::Listen),
            Protocol::Udp => self.remote.is_none(),
        }
    }

    pub fn remote_scope(&self) -> Option<Scope> {
        self.remote.map(|r| Scope::of(r.ip()))
    }

    /// The identity the monitor uses to tell "the same connection" between
    /// two snapshots.
    pub fn key(&self) -> SocketKey {
        SocketKey { pid: self.pid, protocol: self.protocol, local: self.local, remote: self.remote }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SocketKey {
    pub pid: u32,
    pub protocol: Protocol,
    pub local: SocketAddr,
    pub remote: Option<SocketAddr>,
}

/// Every TCP/UDP socket on the system at one moment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Unix time in milliseconds.
    pub taken_at_ms: u64,
    pub processes: Vec<ProcessInfo>,
    pub sockets: Vec<Socket>,
    /// Processes whose sockets we weren't allowed to read (on macOS: other
    /// users' and root's processes unless running as root).
    pub inaccessible: u32,
}

impl Snapshot {
    pub fn new(processes: Vec<ProcessInfo>, sockets: Vec<Socket>, inaccessible: u32) -> Self {
        let taken_at_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        Snapshot { taken_at_ms, processes, sockets, inaccessible }
    }

    pub fn process(&self, pid: u32) -> Option<&ProcessInfo> {
        self.processes.iter().find(|p| p.pid == pid)
    }

    /// Ports each process listens on (TCP) or is bound to without a peer
    /// (UDP), used to tell incoming connections from outgoing ones.
    fn listening_ports(&self) -> HashSet<(u32, Protocol, u16)> {
        self.sockets.iter().filter(|s| s.is_listener()).map(|s| (s.pid, s.protocol, s.local.port())).collect()
    }

    /// Direction of every socket, in `self.sockets` order.
    pub fn directions(&self) -> Vec<Direction> {
        let listening = self.listening_ports();
        self.sockets.iter().map(|s| direction_of(s, &listening)).collect()
    }

    /// Sockets grouped by app (bundle or executable), apps with the most
    /// connections first.
    pub fn by_app(&self) -> Vec<AppConnections> {
        let procs: HashMap<u32, &ProcessInfo> = self.processes.iter().map(|p| (p.pid, p)).collect();
        let listening = self.listening_ports();
        let mut apps: BTreeMap<String, AppConnections> = BTreeMap::new();
        for s in &self.sockets {
            let fallback;
            let p = match procs.get(&s.pid) {
                Some(p) => *p,
                None => {
                    fallback = ProcessInfo { pid: s.pid, name: format!("pid {}", s.pid), path: None };
                    &fallback
                }
            };
            let key = p.app_key();
            let e = apps.entry(key.clone()).or_insert_with(|| AppConnections {
                key,
                name: p.display_name(),
                bundle: p.bundle(),
                path: p.path.clone(),
                pids: Vec::new(),
                connections: Vec::new(),
            });
            if !e.pids.contains(&s.pid) {
                e.pids.push(s.pid);
            }
            e.connections.push(Connection { socket: s.clone(), direction: direction_of(s, &listening), process: p.name.clone() });
        }
        let mut v: Vec<AppConnections> = apps.into_values().collect();
        for a in &mut v {
            a.pids.sort_unstable();
            a.connections.sort_by_key(|c| (c.socket.is_listener(), c.socket.protocol, c.socket.remote, c.socket.local));
        }
        v.sort_by(|a, b| b.active().cmp(&a.active()).then(b.connections.len().cmp(&a.connections.len())).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        v
    }
}

fn direction_of(s: &Socket, listening: &HashSet<(u32, Protocol, u16)>) -> Direction {
    if s.is_listener() {
        return Direction::Incoming;
    }
    // A connected socket on a port this process also listens on was accepted.
    if s.protocol == Protocol::Tcp && listening.contains(&(s.pid, Protocol::Tcp, s.local.port())) {
        Direction::Incoming
    } else {
        Direction::Outgoing
    }
}

/// A socket with what the monitor worked out about it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    #[serde(flatten)]
    pub socket: Socket,
    pub direction: Direction,
    /// The owning process's executable name (a helper's, for bundles).
    pub process: String,
}

/// One app's sockets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConnections {
    pub key: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub pids: Vec<u32>,
    pub connections: Vec<Connection>,
}

impl AppConnections {
    /// Connections with a remote end (not listeners).
    pub fn active(&self) -> usize {
        self.connections.iter().filter(|c| c.socket.remote.is_some()).count()
    }

    pub fn listeners(&self) -> usize {
        self.connections.len() - self.active()
    }

    /// Distinct remote addresses talked to.
    pub fn remotes(&self) -> Vec<IpAddr> {
        let mut v: Vec<IpAddr> = self.connections.iter().filter_map(|c| c.socket.remote.map(|r| canonical_ip(r.ip()))).collect();
        v.sort();
        v.dedup();
        v
    }
}
