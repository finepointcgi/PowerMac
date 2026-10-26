//! Finding other MWB machines on the local network.
//!
//! PowerToys has no discovery of its own (you type the other machine's name),
//! but it accepts a connection from any machine that completes the key
//! handshake and then adds that machine to its layout. So it is enough for one
//! side to find the other: we probe the MWB message port (`port + 1`) on the
//! hosts of each private IPv4 subnet this machine is on, and hand every open
//! socket to the engine's normal client path, where the regular handshake
//! proves (or disproves) that both sides share the security key.
//!
//! * [`local_interfaces`] / [`hosts_for`] / [`lan_candidates`] — which hosts to probe
//!   (RFC 1918 and link-local subnets only, at most [`MAX_HOSTS_PER_INTERFACE`] per
//!   interface, never our own addresses).
//! * [`CandidateProvider`] — where the candidates come from ([`LanScanner`] by
//!   default; tests inject [`StaticCandidates`]).
//! * [`probe`] — bounded-concurrency TCP connects with a short timeout, no retries.
//! * [`Backoff`] — per-host quiet period after a failed handshake (30 min after a
//!   key mismatch), so a machine with another key isn't bothered every sweep.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// At most this many hosts are probed per interface. Larger subnets are
/// scanned only in the /24 around this machine's address.
pub const MAX_HOSTS_PER_INTERFACE: usize = 1024;
/// TCP connect timeout of one probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(300);
/// Probes in flight at once.
pub const PROBE_CONCURRENCY: usize = 64;
/// Quiet period for a host whose handshake showed a different security key.
pub const KEY_MISMATCH_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// Quiet period for a host that accepted the connection but didn't complete
/// the handshake (not an MWB peer, or it hung up).
pub const FAILED_HANDSHAKE_BACKOFF: Duration = Duration::from_secs(10 * 60);

/// An IPv4 address of this machine and its netmask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalV4 {
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
}

/// Private (RFC 1918) or link-local (169.254/16) — the only ranges we scan.
pub fn is_scannable(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local()
}

/// Prefix length of a netmask (`255.255.255.0` -> 24). A non-contiguous mask
/// counts its leading ones only.
pub fn prefix_len(netmask: Ipv4Addr) -> u32 {
    u32::from(netmask).leading_ones()
}

/// The hosts of `iface`'s subnet, without the network and broadcast
/// addresses and without `iface.ip` itself. Subnets with more than `cap`
/// hosts are narrowed to the /24 around `iface.ip`. Empty for addresses we
/// don't scan (public, loopback) and for /32s.
pub fn hosts_for(iface: LocalV4, cap: usize) -> Vec<Ipv4Addr> {
    if !is_scannable(iface.ip) || iface.ip.is_loopback() {
        return Vec::new();
    }
    let mut prefix = prefix_len(iface.netmask);
    if prefix >= 32 {
        return Vec::new();
    }
    let own = u32::from(iface.ip);
    if prefix == 31 {
        // RFC 3021 point-to-point link: both addresses are hosts.
        let other = own ^ 1;
        return vec![Ipv4Addr::from(other)];
    }
    let count = (1u64 << (32 - prefix)) - 2;
    if count > cap as u64 {
        prefix = 24;
    }
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
    let net = own & mask;
    let bcast = net | !mask;
    ((net + 1)..bcast)
        .filter(|&h| h != own)
        .take(cap)
        .map(Ipv4Addr::from)
        .collect()
}

/// This machine's IPv4 interfaces worth scanning: up (or of unknown state),
/// not loopback, not point-to-point (VPN tunnels), with a private or
/// link-local address.
pub fn local_interfaces() -> Vec<LocalV4> {
    let Ok(list) = if_addrs::get_if_addrs() else { return Vec::new() };
    let mut out = Vec::new();
    for i in list {
        if i.is_loopback() || i.is_p2p() {
            continue;
        }
        if !matches!(i.oper_status, if_addrs::IfOperStatus::Up | if_addrs::IfOperStatus::Unknown) {
            continue;
        }
        if let if_addrs::IfAddr::V4(a) = &i.addr {
            if is_scannable(a.ip) {
                out.push(LocalV4 { ip: a.ip, netmask: a.netmask });
            }
        }
    }
    out
}

/// All of this machine's IPv4 addresses (including loopback and public ones).
pub fn own_addresses() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .map(|l| {
            l.into_iter()
                .filter_map(|i| match i.addr {
                    if_addrs::IfAddr::V4(a) => Some(a.ip),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Hosts to probe for `ifaces`: each interface's [`hosts_for`], merged
/// (overlapping subnets are probed once) and without any of `own`.
pub fn lan_candidates(ifaces: &[LocalV4], own: &[Ipv4Addr], cap: usize) -> Vec<Ipv4Addr> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for i in ifaces {
        for h in hosts_for(*i, cap) {
            if !own.contains(&h) && seen.insert(h) {
                out.push(h);
            }
        }
    }
    out
}

/// Where discovery gets the addresses to probe.
pub trait CandidateProvider: Send + Sync {
    /// Socket addresses to probe; `msg_port` is the MWB message port (`port + 1`).
    fn candidates(&self, msg_port: u16) -> Vec<SocketAddr>;
}

/// The default provider: every host of this machine's private subnets.
#[derive(Debug, Default, Clone, Copy)]
pub struct LanScanner;

impl CandidateProvider for LanScanner {
    fn candidates(&self, msg_port: u16) -> Vec<SocketAddr> {
        let own = own_addresses();
        lan_candidates(&local_interfaces(), &own, MAX_HOSTS_PER_INTERFACE)
            .into_iter()
            .map(|ip| SocketAddr::new(IpAddr::V4(ip), msg_port))
            .collect()
    }
}

/// A fixed list of addresses (tests, or a "known hosts" list).
#[derive(Debug, Default, Clone)]
pub struct StaticCandidates(pub Vec<SocketAddr>);

impl CandidateProvider for StaticCandidates {
    fn candidates(&self, _msg_port: u16) -> Vec<SocketAddr> {
        self.0.clone()
    }
}

/// Connect to each of `addrs` (at most `concurrency` at a time, `timeout`
/// each, no retries) and call `on_open` with every socket that connected, as
/// soon as it does. Stops early when `stop` is set. Returns how many were open.
pub fn probe(
    addrs: &[SocketAddr],
    timeout: Duration,
    concurrency: usize,
    stop: &AtomicBool,
    on_open: &(dyn Fn(SocketAddr, TcpStream) + Sync),
) -> usize {
    let next = AtomicUsize::new(0);
    let open = AtomicUsize::new(0);
    let workers = concurrency.max(1).min(addrs.len());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(a) = addrs.get(i) else { break };
                if let Ok(st) = TcpStream::connect_timeout(a, timeout) {
                    open.fetch_add(1, Ordering::SeqCst);
                    on_open(*a, st);
                }
            });
        }
    });
    open.load(Ordering::SeqCst)
}

/// Why a host is being left alone for a while.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackoffReason {
    /// The handshake failed: the host uses another security key.
    KeyMismatch,
    /// The host accepted the connection but no handshake completed.
    NoHandshake,
}

impl BackoffReason {
    pub fn duration(self) -> Duration {
        match self {
            BackoffReason::KeyMismatch => KEY_MISMATCH_BACKOFF,
            BackoffReason::NoHandshake => FAILED_HANDSHAKE_BACKOFF,
        }
    }
}

/// Per-host quiet periods.
#[derive(Debug, Default)]
pub struct Backoff {
    until: HashMap<SocketAddr, (Instant, BackoffReason)>,
}

impl Backoff {
    /// Leave `addr` alone for `reason.duration()` from `now`.
    pub fn block(&mut self, addr: SocketAddr, reason: BackoffReason, now: Instant) {
        self.until.insert(addr, (now + reason.duration(), reason));
    }

    /// Whether `addr` is still in its quiet period at `now`.
    pub fn is_blocked(&self, addr: &SocketAddr, now: Instant) -> bool {
        self.until.get(addr).is_some_and(|(t, _)| now < *t)
    }

    pub fn reason(&self, addr: &SocketAddr, now: Instant) -> Option<BackoffReason> {
        self.until.get(addr).filter(|(t, _)| now < *t).map(|(_, r)| *r)
    }

    /// Hosts blocked for `reason` at `now`, sorted.
    pub fn blocked(&self, reason: BackoffReason, now: Instant) -> Vec<SocketAddr> {
        let mut v: Vec<_> = self.until.iter().filter(|(_, (t, r))| *r == reason && now < *t).map(|(a, _)| *a).collect();
        v.sort();
        v
    }

    /// Forget `addr` (it completed a handshake).
    pub fn clear(&mut self, addr: &SocketAddr) {
        self.until.remove(addr);
    }

    /// Drop expired entries.
    pub fn prune(&mut self, now: Instant) {
        self.until.retain(|_, (t, _)| now < *t);
    }

    pub fn len(&self) -> usize {
        self.until.len()
    }

    pub fn is_empty(&self) -> bool {
        self.until.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    fn iface(ip: &str, mask: &str) -> LocalV4 {
        LocalV4 { ip: v4(ip), netmask: v4(mask) }
    }

    #[test]
    fn private_ranges_only() {
        for ok in ["10.0.0.1", "10.255.1.2", "172.16.0.1", "172.31.255.254", "192.168.1.20", "169.254.3.4"] {
            assert!(is_scannable(v4(ok)), "{ok}");
        }
        for no in ["8.8.8.8", "172.15.0.1", "172.32.0.1", "192.169.0.1", "127.0.0.1", "100.64.0.1", "224.0.0.1", "0.0.0.0"] {
            assert!(!is_scannable(v4(no)), "{no}");
        }
        assert!(hosts_for(iface("8.8.8.8", "255.255.255.0"), 1024).is_empty());
        assert!(hosts_for(iface("127.0.0.1", "255.0.0.0"), 1024).is_empty());
    }

    #[test]
    fn prefix_lengths() {
        assert_eq!(prefix_len(v4("255.255.255.0")), 24);
        assert_eq!(prefix_len(v4("255.255.252.0")), 22);
        assert_eq!(prefix_len(v4("255.0.0.0")), 8);
        assert_eq!(prefix_len(v4("255.255.255.255")), 32);
        assert_eq!(prefix_len(v4("0.0.0.0")), 0);
    }

    #[test]
    fn slash_24_hosts() {
        let h = hosts_for(iface("192.168.1.20", "255.255.255.0"), 1024);
        assert_eq!(h.len(), 253); // .1-.254 without ourselves
        assert_eq!(h.first(), Some(&v4("192.168.1.1")));
        assert_eq!(h.last(), Some(&v4("192.168.1.254")));
        assert!(!h.contains(&v4("192.168.1.20")));
        assert!(!h.contains(&v4("192.168.1.0")) && !h.contains(&v4("192.168.1.255")));
    }

    #[test]
    fn small_and_point_to_point_subnets() {
        assert_eq!(hosts_for(iface("10.0.0.5", "255.255.255.252"), 1024), vec![v4("10.0.0.6")]);
        assert_eq!(hosts_for(iface("10.0.0.4", "255.255.255.254"), 1024), vec![v4("10.0.0.5")]);
        assert_eq!(hosts_for(iface("10.0.0.5", "255.255.255.254"), 1024), vec![v4("10.0.0.4")]);
        assert!(hosts_for(iface("10.0.0.5", "255.255.255.255"), 1024).is_empty());
    }

    #[test]
    fn cap_applies_and_large_subnets_scan_the_slash_24() {
        // /22 = 1022 hosts: under the cap, scanned whole.
        let h = hosts_for(iface("172.20.5.9", "255.255.252.0"), 1024);
        assert_eq!(h.len(), 1021);
        assert_eq!(h.first(), Some(&v4("172.20.4.1")));
        assert_eq!(h.last(), Some(&v4("172.20.7.254")));
        // /21 = 2046 hosts: over the cap, only 172.20.5.0/24.
        let h = hosts_for(iface("172.20.5.9", "255.255.248.0"), 1024);
        assert_eq!(h.len(), 253);
        assert!(h.iter().all(|a| a.octets()[..3] == [172, 20, 5]));
        // 10/8 and link-local /16 too.
        let h = hosts_for(iface("10.1.2.3", "255.0.0.0"), 1024);
        assert_eq!(h.len(), 253);
        assert!(h.iter().all(|a| a.octets()[..3] == [10, 1, 2]));
        let h = hosts_for(iface("169.254.77.1", "255.255.0.0"), 1024);
        assert_eq!(h.len(), 253);
        // A smaller cap truncates.
        assert_eq!(hosts_for(iface("192.168.0.1", "255.255.255.0"), 16).len(), 16);
        // Every interface stays within the global cap.
        for mask in ["255.255.0.0", "255.255.240.0", "255.255.254.0", "255.255.255.128"] {
            assert!(hosts_for(iface("192.168.3.3", mask), MAX_HOSTS_PER_INTERFACE).len() <= MAX_HOSTS_PER_INTERFACE);
        }
    }

    #[test]
    fn candidates_merge_and_skip_own_addresses() {
        let ifaces = [
            iface("192.168.1.20", "255.255.255.0"),
            iface("192.168.1.21", "255.255.255.0"), // second address on the same subnet
            iface("10.0.0.1", "255.255.255.252"),
            iface("8.8.8.8", "255.255.255.0"), // public: ignored
        ];
        let own = [v4("192.168.1.20"), v4("192.168.1.21"), v4("10.0.0.1"), v4("127.0.0.1")];
        let c = lan_candidates(&ifaces, &own, 1024);
        assert_eq!(c.len(), 252 + 1);
        assert!(!c.contains(&v4("192.168.1.20")) && !c.contains(&v4("192.168.1.21")));
        assert!(c.contains(&v4("10.0.0.2")));
        let unique: BTreeSet<_> = c.iter().collect();
        assert_eq!(unique.len(), c.len());
    }

    #[test]
    fn backoff_per_host() {
        let a: SocketAddr = "192.168.1.30:15101".parse().unwrap();
        let b: SocketAddr = "192.168.1.31:15101".parse().unwrap();
        let t0 = Instant::now();
        let mut bo = Backoff::default();
        assert!(!bo.is_blocked(&a, t0));
        bo.block(a, BackoffReason::KeyMismatch, t0);
        bo.block(b, BackoffReason::NoHandshake, t0);
        assert!(bo.is_blocked(&a, t0 + Duration::from_secs(29 * 60)));
        assert_eq!(bo.reason(&a, t0), Some(BackoffReason::KeyMismatch));
        assert!(!bo.is_blocked(&a, t0 + KEY_MISMATCH_BACKOFF));
        assert!(bo.is_blocked(&b, t0 + Duration::from_secs(9 * 60)));
        assert!(!bo.is_blocked(&b, t0 + Duration::from_secs(10 * 60)));
        assert_eq!(bo.blocked(BackoffReason::KeyMismatch, t0), vec![a]);
        assert_eq!(bo.blocked(BackoffReason::NoHandshake, t0), vec![b]);
        bo.prune(t0 + Duration::from_secs(11 * 60));
        assert_eq!(bo.len(), 1);
        bo.clear(&a);
        assert!(bo.is_empty());
    }

    #[test]
    fn probe_finds_open_loopback_ports_only() {
        // Loopback only: one listening port and one closed port on this machine.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = l.local_addr().unwrap();
        let closed = {
            let t = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            t.local_addr().unwrap()
        };
        let found = std::sync::Mutex::new(Vec::new());
        let stop = AtomicBool::new(false);
        let n = probe(&[closed, open], PROBE_TIMEOUT, 4, &stop, &|a, _s| found.lock().unwrap().push(a));
        assert_eq!(n, 1);
        assert_eq!(*found.lock().unwrap(), vec![open]);
        // Nothing to probe: no threads, nothing found.
        assert_eq!(probe(&[], PROBE_TIMEOUT, 4, &stop, &|_, _| {}), 0);
    }
}
