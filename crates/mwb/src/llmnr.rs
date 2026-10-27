//! A small LLMNR responder (RFC 4795) so Windows finds this machine by its
//! Mouse Without Borders name.
//!
//! PowerToys connects to the names in its machine matrix with
//! `Dns.GetHostEntry(name)` (`SocketStuff.StartNewTcpClient`). On Windows that
//! is `getaddrinfo`, which for a single-label name like `MACBOOK-PRO` asks DNS
//! (usually no answer for a Mac on a home network), then LLMNR and NetBIOS.
//! macOS answers neither LLMNR nor (without SMB sharing) NetBIOS, so a PC
//! can't find a Mac by name: "Connect" in PowerToys silently does nothing.
//! This responder answers LLMNR queries for our MWB name:
//!
//! * `A` (and `ANY`): the Mac's IPv4 address on the interface the query came
//!   in on (the address in the sender's subnet; for IPv6 link-local senders,
//!   the IPv4 address of the interface with the sender's scope id);
//! * `AAAA` and other types for our name: an empty answer (we're authoritative
//!   for the name but only offer IPv4, which is all our listener binds);
//! * `PTR` for `d.c.b.a.in-addr.arpa` of one of our IPv4 addresses: our name.
//!
//! Names match ASCII case-insensitively, with or without a `.local` suffix.
//! It listens on UDP 5355 (unprivileged) on 224.0.0.252 and ff02::1:3 (joined
//! on every interface) and answers unicast queries too. NetBIOS name service
//! (UDP 137) would need root and is not implemented.

use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use log::{debug, info, warn};

/// The LLMNR UDP port.
pub const PORT: u16 = 5355;
/// IPv4 link-scope multicast group.
pub const GROUP_V4: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 252);
/// IPv6 link-scope multicast group.
pub const GROUP_V6: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 1, 3);
/// TTL of our answers (the RFC's suggested default).
pub const ANSWER_TTL: u32 = 30;
/// Whether [`Responder::start`] works on this platform. Windows runs its own
/// LLMNR responder for the computer name (and holds the port).
pub const SUPPORTED: bool = cfg!(unix);

/// Resource record types we look at.
pub mod rtype {
    pub const A: u16 = 1;
    pub const PTR: u16 = 12;
    pub const AAAA: u16 = 28;
    pub const ANY: u16 = 255;
}

const CLASS_IN: u16 = 1;
const CLASS_ANY: u16 = 255;
const FLAG_QR: u16 = 0x8000;
const OPCODE_MASK: u16 = 0x7800;
/// How often the multicast memberships are refreshed (interfaces come and go).
const REJOIN_EVERY: Duration = Duration::from_secs(30);
const RECV_TIMEOUT: Duration = Duration::from_millis(250);

/// A parsed LLMNR query (one question).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub id: u16,
    pub flags: u16,
    /// The question name, labels joined with `.` (no trailing dot).
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
    /// The question section as received (copied into the response).
    pub question: Vec<u8>,
}

fn be16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

/// Parse an LLMNR query. `None` for anything a responder must silently drop
/// (RFC 4795 2.1.1): responses, other opcodes, QDCOUNT != 1, answers or
/// authority records present, compressed or malformed names.
pub fn parse_query(buf: &[u8]) -> Option<Query> {
    if buf.len() < 12 {
        return None;
    }
    let id = be16(buf, 0)?;
    let flags = be16(buf, 2)?;
    if flags & FLAG_QR != 0 || flags & OPCODE_MASK != 0 {
        return None;
    }
    if be16(buf, 4)? != 1 || be16(buf, 6)? != 0 || be16(buf, 8)? != 0 {
        return None;
    }
    let mut i = 12;
    let mut labels: Vec<&str> = Vec::new();
    loop {
        let len = *buf.get(i)? as usize;
        i += 1;
        if len == 0 {
            break;
        }
        if len > 63 {
            return None; // compression pointers don't belong in a query's question
        }
        let label = std::str::from_utf8(buf.get(i..i + len)?).ok()?;
        if label.contains('.') {
            return None;
        }
        labels.push(label);
        i += len;
        if i - 12 > 255 {
            return None;
        }
    }
    let qtype = be16(buf, i)?;
    let qclass = be16(buf, i + 2)?;
    Some(Query { id, flags, name: labels.join("."), qtype, qclass, question: buf[12..i + 4].to_vec() })
}

/// Whether the queried `qname` is `ours` (ASCII case-insensitive, optional
/// `.local` suffix, optional trailing dot).
pub fn name_matches(qname: &str, ours: &str) -> bool {
    let q = qname.trim_end_matches('.');
    let ours = ours.trim();
    if ours.is_empty() {
        return false;
    }
    if q.eq_ignore_ascii_case(ours) {
        return true;
    }
    q.len() == ours.len() + 6
        && q.is_char_boundary(ours.len())
        && q[..ours.len()].eq_ignore_ascii_case(ours)
        && q[ours.len()..].eq_ignore_ascii_case(".local")
}

/// `d.c.b.a.in-addr.arpa` -> `a.b.c.d`.
pub fn reverse_v4(qname: &str) -> Option<Ipv4Addr> {
    let q = qname.trim_end_matches('.');
    const SUFFIX: &str = ".in-addr.arpa";
    if q.len() <= SUFFIX.len() || !q.is_char_boundary(q.len() - SUFFIX.len()) {
        return None;
    }
    let (head, tail) = q.split_at(q.len() - SUFFIX.len());
    if !tail.eq_ignore_ascii_case(SUFFIX) {
        return None;
    }
    let parts: Vec<u8> = head.split('.').map(|p| p.parse::<u8>().ok()).collect::<Option<_>>()?;
    match parts[..] {
        [d, c, b, a] => Some(Ipv4Addr::new(a, b, c, d)),
        _ => None,
    }
}

/// One answer record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    A(Ipv4Addr),
    Ptr(String),
}

/// What to answer for `q`: `None` = not our name (stay silent), `Some(empty)` =
/// our name but no record of that type. `reply_ip` is the address for an
/// `A` answer (`None`: we have no usable address, stay silent); `own_v4` the
/// addresses we answer `PTR` queries for.
pub fn answer_for(q: &Query, name: &str, reply_ip: Option<Ipv4Addr>, own_v4: &[Ipv4Addr]) -> Option<Vec<Answer>> {
    if q.qclass & 0x7FFF != CLASS_IN && q.qclass & 0x7FFF != CLASS_ANY {
        return None;
    }
    if name_matches(&q.name, name) {
        return match q.qtype {
            rtype::A | rtype::ANY => reply_ip.map(|ip| vec![Answer::A(ip)]),
            _ => Some(Vec::new()),
        };
    }
    if let Some(ip) = reverse_v4(&q.name) {
        if own_v4.contains(&ip) && matches!(q.qtype, rtype::PTR | rtype::ANY) {
            return Some(vec![Answer::Ptr(name.trim().to_string())]);
        }
    }
    None
}

/// Encode a dotted name as DNS labels (uncompressed).
pub fn encode_name(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.trim_end_matches('.').split('.').filter(|l| !l.is_empty()) {
        let b = label.as_bytes();
        let n = b.len().min(63);
        out.push(n as u8);
        out.extend_from_slice(&b[..n]);
    }
    out.push(0);
    out
}

/// The response packet for `q` with `answers` (QR set, RCODE 0, the question
/// echoed, answer names written out in full).
pub fn build_response(q: &Query, answers: &[Answer]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&q.id.to_be_bytes());
    out.extend_from_slice(&FLAG_QR.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(answers.len() as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&q.question);
    // The question name as received (case preserved).
    let qname = &q.question[..q.question.len() - 4];
    for a in answers {
        out.extend_from_slice(qname);
        let (t, rdata) = match a {
            Answer::A(ip) => (rtype::A, ip.octets().to_vec()),
            Answer::Ptr(n) => (rtype::PTR, encode_name(n)),
        };
        out.extend_from_slice(&t.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&ANSWER_TTL.to_be_bytes());
        out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        out.extend_from_slice(&rdata);
    }
    out
}

/// A query packet (for `powermac mwb doctor` and the tests).
pub fn build_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&encode_name(name));
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    out
}

/// The `A` records in a response to query `id` (empty for an empty answer;
/// `None` if `buf` isn't a response to `id`).
pub fn parse_a_answers(buf: &[u8], id: u16) -> Option<Vec<Ipv4Addr>> {
    if be16(buf, 0)? != id || be16(buf, 2)? & FLAG_QR == 0 {
        return None;
    }
    let qd = be16(buf, 4)?;
    let an = be16(buf, 6)?;
    let mut i = 12;
    let skip_name = |mut i: usize| -> Option<usize> {
        loop {
            let len = *buf.get(i)? as usize;
            if len & 0xC0 == 0xC0 {
                return Some(i + 2);
            }
            i += 1 + len;
            if len == 0 {
                return Some(i);
            }
        }
    };
    for _ in 0..qd {
        i = skip_name(i)? + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        i = skip_name(i)?;
        let t = be16(buf, i)?;
        let len = be16(buf, i + 8)? as usize;
        let rdata = buf.get(i + 10..i + 10 + len)?;
        if t == rtype::A && len == 4 {
            out.push(Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]));
        }
        i += 10 + len;
    }
    Some(out)
}

/// Send one LLMNR `A` query for `name` to `target` and wait for the answer.
/// `Ok(None)`: no response within `timeout`.
pub fn query_a(name: &str, target: SocketAddr, timeout: Duration) -> io::Result<Option<Vec<Ipv4Addr>>> {
    let bind: SocketAddr = if target.is_ipv4() { "0.0.0.0:0".parse().unwrap() } else { "[::]:0".parse().unwrap() };
    let s = UdpSocket::bind(bind)?;
    s.set_read_timeout(Some(timeout))?;
    let id: u16 = rand::random();
    s.send_to(&build_query(id, name, rtype::A), target)?;
    let end = Instant::now() + timeout;
    let mut buf = [0u8; 1500];
    while Instant::now() < end {
        match s.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Some(a) = parse_a_answers(&buf[..n], id) {
                    return Ok(Some(a));
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

// ----------------------------------------------------------------------
// Choosing the address to answer with
// ----------------------------------------------------------------------

/// An IPv4 address of this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalIf {
    pub index: Option<u32>,
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    /// Point-to-point (VPN tunnels such as NordVPN's utun).
    pub p2p: bool,
}

/// This machine's IPv4 addresses on interfaces that are up.
pub fn local_ifs() -> Vec<LocalIf> {
    let Ok(list) = if_addrs::get_if_addrs() else { return Vec::new() };
    list.into_iter()
        .filter(|i| matches!(i.oper_status, if_addrs::IfOperStatus::Up | if_addrs::IfOperStatus::Unknown))
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) => Some(LocalIf { index: i.index, ip: a.ip, netmask: a.netmask, p2p: i.is_p2p }),
            _ => None,
        })
        .collect()
}

fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, mask: Ipv4Addr) -> bool {
    let m = u32::from(mask);
    m != 0 && u32::from(a) & m == u32::from(b) & m
}

/// The "main" LAN address: a private / link-local address on a regular
/// (not point-to-point) interface, private preferred.
pub fn primary_ipv4(ifs: &[LocalIf]) -> Option<Ipv4Addr> {
    let usable = |i: &&LocalIf| !i.ip.is_loopback() && !i.p2p && !i.ip.is_unspecified();
    ifs.iter()
        .filter(usable)
        .find(|i| i.ip.is_private())
        .or_else(|| ifs.iter().filter(usable).find(|i| i.ip.is_link_local()))
        .or_else(|| ifs.iter().find(usable))
        .map(|i| i.ip)
}

/// The address to put in an `A` answer for a query from `src`: ours on the
/// sender's subnet, or (IPv6 link-local sender) the IPv4 address of the
/// interface the query came in on; for loopback / unmatched senders the
/// primary LAN address. Tunnel (p2p) addresses only when nothing else matches.
pub fn pick_reply_ip(src: SocketAddr, ifs: &[LocalIf]) -> Option<Ipv4Addr> {
    let regular = |i: &&LocalIf| !i.ip.is_loopback() && !i.p2p;
    let v4 = match src.ip() {
        IpAddr::V4(v) => Some(v),
        IpAddr::V6(v) => v.to_ipv4_mapped(),
    };
    if let Some(v) = v4 {
        if !v.is_loopback() {
            if let Some(i) = ifs.iter().filter(regular).find(|i| i.ip == v || same_subnet(i.ip, v, i.netmask)) {
                return Some(i.ip);
            }
            if let Some(i) = ifs.iter().find(|i| !i.ip.is_loopback() && same_subnet(i.ip, v, i.netmask)) {
                return Some(i.ip);
            }
        }
    } else if let SocketAddr::V6(s6) = src {
        if s6.scope_id() != 0 {
            let on_if: Vec<&LocalIf> = ifs.iter().filter(|i| i.index == Some(s6.scope_id()) && !i.ip.is_loopback()).collect();
            if let Some(i) = on_if.iter().find(|i| i.ip.is_private()).or(on_if.first()) {
                return Some(i.ip);
            }
        }
    }
    primary_ipv4(ifs)
}

// ----------------------------------------------------------------------
// The responder
// ----------------------------------------------------------------------

/// What the responder has been doing (shown by `powermac mwb doctor`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LlmnrStatus {
    pub running: bool,
    pub name: String,
    pub port: u16,
    pub ipv4: bool,
    pub ipv6: bool,
    /// IPv4 interfaces whose 224.0.0.252 membership is active.
    pub groups_v4: Vec<Ipv4Addr>,
    /// Queries received (any name).
    pub queries: u64,
    /// Queries for our name (or our addresses) that were answered.
    pub answered: u64,
    /// Last answered query: (sender, queried name, answer, when).
    pub last_answer: Option<(SocketAddr, String, Option<Ipv4Addr>, SystemTime)>,
    /// Last query for another name (sender, name).
    pub last_other: Option<(SocketAddr, String)>,
    pub error: Option<String>,
}

/// A running LLMNR responder. Dropping it stops it.
pub struct Responder {
    stop: Arc<AtomicBool>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    status: Arc<Mutex<LlmnrStatus>>,
}

impl Responder {
    /// Answer for `name` on UDP `port` (normally [`PORT`]; 0 = any free port,
    /// for tests). Fails if the IPv4 socket can't be bound; IPv6 is best effort.
    pub fn start(name: &str, port: u16) -> io::Result<Responder> {
        Self::start_on(name, Ipv4Addr::UNSPECIFIED, Some(Ipv6Addr::UNSPECIFIED), port)
    }

    /// [`Responder::start`] on specific addresses. Multicast groups are joined
    /// only when bound to the unspecified address; tests bind loopback (no
    /// multicast, and nothing the macOS firewall has to allow).
    pub fn start_on(name: &str, v4_addr: Ipv4Addr, v6_addr: Option<Ipv6Addr>, port: u16) -> io::Result<Responder> {
        if !SUPPORTED {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "the OS answers LLMNR for its own name here"));
        }
        let name = name.trim().to_string();
        let multicast = v4_addr.is_unspecified();
        let v4 = UdpSocket::bind((v4_addr, port))?;
        let port = v4.local_addr()?.port();
        v4.set_read_timeout(Some(RECV_TIMEOUT))?;
        let v6 = match v6_addr.map(|a| bind_v6_only(a, port)) {
            Some(Ok(s)) => Some(s),
            Some(Err(e)) => {
                debug!("llmnr: no IPv6 socket: {e}");
                None
            }
            None => None,
        };
        let status = Arc::new(Mutex::new(LlmnrStatus {
            running: true,
            name: name.clone(),
            port,
            ipv4: true,
            ipv6: v6.is_some(),
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        {
            let (stop, status, name) = (stop.clone(), status.clone(), name.clone());
            threads.push(std::thread::Builder::new().name("mwb-llmnr4".into()).spawn(move || serve(v4, false, multicast, &name, &stop, &status))?);
        }
        if let Some(v6) = v6 {
            v6.set_read_timeout(Some(RECV_TIMEOUT))?;
            let (stop, status, name) = (stop.clone(), status.clone(), name.clone());
            threads.push(std::thread::Builder::new().name("mwb-llmnr6".into()).spawn(move || serve(v6, true, multicast, &name, &stop, &status))?);
        }
        info!("llmnr: answering name queries for '{name}' on UDP {port}");
        Ok(Responder { stop, threads: Mutex::new(threads), status })
    }

    pub fn status(&self) -> LlmnrStatus {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn port(&self) -> u16 {
        self.status().port
    }

    /// Stop and wait for the threads (at most a few hundred ms).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let threads: Vec<_> = self.threads.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        for t in threads {
            let _ = t.join();
        }
        self.status.lock().unwrap_or_else(|e| e.into_inner()).running = false;
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(unix)]
fn bind_v6_only(addr: Ipv6Addr, port: u16) -> io::Result<UdpSocket> {
    use std::os::fd::FromRawFd;
    // std can't set IPV6_V6ONLY before bind; a dual-stack socket would
    // collide with the IPv4 one on the same port.
    unsafe {
        let fd = libc::socket(libc::AF_INET6, libc::SOCK_DGRAM, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let sock = UdpSocket::from_raw_fd(fd);
        let one: libc::c_int = 1;
        if libc::setsockopt(
            fd,
            libc::IPPROTO_IPV6,
            libc::IPV6_V6ONLY,
            &one as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut sa: libc::sockaddr_in6 = std::mem::zeroed();
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
        {
            sa.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
        }
        sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
        sa.sin6_port = port.to_be();
        sa.sin6_addr.s6_addr = addr.octets();
        if libc::bind(fd, &sa as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(sock)
    }
}

#[cfg(not(unix))]
fn bind_v6_only(_addr: Ipv6Addr, _port: u16) -> io::Result<UdpSocket> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "not implemented"))
}

/// Join the LLMNR group on every interface not joined yet.
fn join_groups(sock: &UdpSocket, v6: bool, joined: &mut HashSet<String>, status: &Mutex<LlmnrStatus>) {
    let Ok(list) = if_addrs::get_if_addrs() else { return };
    for i in list {
        if i.is_loopback() || !matches!(i.oper_status, if_addrs::IfOperStatus::Up | if_addrs::IfOperStatus::Unknown) {
            continue;
        }
        match (&i.addr, v6) {
            (if_addrs::IfAddr::V4(a), false) => {
                let key = a.ip.to_string();
                if joined.contains(&key) {
                    continue;
                }
                match sock.join_multicast_v4(&GROUP_V4, &a.ip) {
                    Ok(()) => {
                        debug!("llmnr: joined {GROUP_V4} on {} ({})", i.name, a.ip);
                        joined.insert(key);
                        status.lock().unwrap_or_else(|e| e.into_inner()).groups_v4.push(a.ip);
                    }
                    Err(e) => debug!("llmnr: join {GROUP_V4} on {} ({}): {e}", i.name, a.ip),
                }
            }
            (if_addrs::IfAddr::V6(a), true) if a.ip.is_unicast_link_local() => {
                let Some(idx) = i.index else { continue };
                let key = idx.to_string();
                if joined.contains(&key) {
                    continue;
                }
                match sock.join_multicast_v6(&GROUP_V6, idx) {
                    Ok(()) => {
                        debug!("llmnr: joined {GROUP_V6} on {}", i.name);
                        joined.insert(key);
                    }
                    Err(e) => debug!("llmnr: join {GROUP_V6} on {}: {e}", i.name),
                }
            }
            _ => {}
        }
    }
}

fn serve(sock: UdpSocket, v6: bool, multicast: bool, name: &str, stop: &AtomicBool, status: &Mutex<LlmnrStatus>) {
    let mut joined = HashSet::new();
    let mut last_join: Option<Instant> = None;
    let mut ifs = Vec::new();
    let mut ifs_at: Option<Instant> = None;
    let mut buf = [0u8; 1500];
    while !stop.load(Ordering::SeqCst) {
        if multicast && last_join.is_none_or(|t| t.elapsed() >= REJOIN_EVERY) {
            last_join = Some(Instant::now());
            join_groups(&sock, v6, &mut joined, status);
        }
        let (n, src) = match sock.recv_from(&mut buf) {
            Ok(r) => r,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => continue,
            Err(e) => {
                warn!("llmnr: receive failed: {e}");
                status.lock().unwrap_or_else(|e| e.into_inner()).error = Some(e.to_string());
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let Some(q) = parse_query(&buf[..n]) else { continue };
        if ifs_at.is_none_or(|t: Instant| t.elapsed() >= Duration::from_secs(5)) {
            ifs = local_ifs();
            ifs_at = Some(Instant::now());
        }
        let reply_ip = pick_reply_ip(src, &ifs);
        let own: Vec<Ipv4Addr> = ifs.iter().map(|i| i.ip).collect();
        let answers = answer_for(&q, name, reply_ip, &own);
        {
            let mut st = status.lock().unwrap_or_else(|e| e.into_inner());
            st.queries += 1;
            if answers.is_none() {
                st.last_other = Some((src, q.name.clone()));
            }
        }
        let Some(answers) = answers else { continue };
        let resp = build_response(&q, &answers);
        match sock.send_to(&resp, src) {
            Ok(_) => {
                let a = answers.iter().find_map(|a| match a {
                    Answer::A(ip) => Some(*ip),
                    _ => None,
                });
                debug!("llmnr: {} asked for {} (type {}): answered {:?}", src, q.name, q.qtype, answers);
                let mut st = status.lock().unwrap_or_else(|e| e.into_inner());
                st.answered += 1;
                st.last_answer = Some((src, q.name.clone(), a, SystemTime::now()));
            }
            Err(e) => debug!("llmnr: reply to {src}: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Windows-style query: `MACBOOK-PRO`, type A, class IN.
    fn win_query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = build_query(0xBEEF, name, qtype);
        // Windows sends ARCOUNT 0 and flags 0; exercise C bit / EDNS-free path.
        q[2] = 0;
        q
    }

    #[test]
    fn parses_a_query() {
        let q = parse_query(&win_query("MACBOOK-PRO", rtype::A)).unwrap();
        assert_eq!(q.id, 0xBEEF);
        assert_eq!(q.name, "MACBOOK-PRO");
        assert_eq!(q.qtype, rtype::A);
        assert_eq!(q.qclass, 1);
        assert_eq!(q.question.len(), 1 + 11 + 1 + 4);
    }

    #[test]
    fn drops_what_must_be_dropped() {
        let mut r = win_query("X", rtype::A);
        r[2] |= 0x80; // QR
        assert!(parse_query(&r).is_none());
        let mut op = win_query("X", rtype::A);
        op[2] |= 0x08; // opcode 1
        assert!(parse_query(&op).is_none());
        let mut qd = win_query("X", rtype::A);
        qd[5] = 2;
        assert!(parse_query(&qd).is_none());
        let mut an = win_query("X", rtype::A);
        an[7] = 1;
        assert!(parse_query(&an).is_none());
        assert!(parse_query(&[0; 11]).is_none());
        let mut ptr = win_query("X", rtype::A);
        ptr[12] = 0xC0;
        assert!(parse_query(&ptr).is_none());
        let full = win_query("MACBOOK-PRO", rtype::A);
        assert!(parse_query(&full[..full.len() - 1]).is_none());
        // The C (conflict) bit is fine.
        let mut c = win_query("X", rtype::A);
        c[2] |= 0x04;
        assert!(parse_query(&c).is_some());
    }

    #[test]
    fn name_matching() {
        assert!(name_matches("MACBOOK-PRO", "MacBook-Pro"));
        assert!(name_matches("macbook-pro", "MacBook-Pro"));
        assert!(name_matches("MacBook-Pro.local", "MacBook-Pro"));
        assert!(name_matches("MACBOOK-PRO.LOCAL.", "MacBook-Pro"));
        assert!(!name_matches("MACBOOK-PRO2", "MacBook-Pro"));
        assert!(!name_matches("MACBOOK", "MacBook-Pro"));
        assert!(!name_matches("MacBook-Pro.lan", "MacBook-Pro"));
        assert!(!name_matches("x", ""));
    }

    #[test]
    fn reverse_names() {
        assert_eq!(reverse_v4("112.0.0.10.in-addr.arpa"), Some(Ipv4Addr::new(10, 0, 0, 112)));
        assert_eq!(reverse_v4("112.0.0.10.IN-ADDR.ARPA."), Some(Ipv4Addr::new(10, 0, 0, 112)));
        assert_eq!(reverse_v4("0.0.10.in-addr.arpa"), None);
        assert_eq!(reverse_v4("300.0.0.10.in-addr.arpa"), None);
        assert_eq!(reverse_v4("in-addr.arpa"), None);
    }

    #[test]
    fn answers() {
        let mac = Ipv4Addr::new(10, 0, 0, 112);
        let ip = Some(mac);
        let own = [Ipv4Addr::new(10, 0, 0, 112), Ipv4Addr::LOCALHOST];
        let q = |n: &str, t| parse_query(&win_query(n, t)).unwrap();
        assert_eq!(answer_for(&q("MACBOOK-PRO", rtype::A), "MacBook-Pro", ip, &own), Some(vec![Answer::A(mac)]));
        assert_eq!(answer_for(&q("MACBOOK-PRO", rtype::ANY), "MacBook-Pro", ip, &own), Some(vec![Answer::A(mac)]));
        assert_eq!(answer_for(&q("MACBOOK-PRO", rtype::AAAA), "MacBook-Pro", ip, &own), Some(vec![]));
        assert_eq!(answer_for(&q("MACBOOK-PRO", rtype::A), "MacBook-Pro", None, &own), None);
        assert_eq!(answer_for(&q("DESKTOP-PC", rtype::A), "MacBook-Pro", ip, &own), None);
        assert_eq!(
            answer_for(&q("112.0.0.10.in-addr.arpa", rtype::PTR), "MacBook-Pro", ip, &own),
            Some(vec![Answer::Ptr("MacBook-Pro".into())])
        );
        assert_eq!(answer_for(&q("35.0.0.10.in-addr.arpa", rtype::PTR), "MacBook-Pro", ip, &own), None);
    }

    #[test]
    fn response_bytes() {
        let q = parse_query(&win_query("MACBOOK-PRO", rtype::A)).unwrap();
        let r = build_response(&q, &[Answer::A(Ipv4Addr::new(10, 0, 0, 112))]);
        let mut want = vec![0xBE, 0xEF, 0x80, 0x00, 0, 1, 0, 1, 0, 0, 0, 0];
        let name = [&[11u8][..], b"MACBOOK-PRO", &[0]].concat();
        want.extend_from_slice(&name);
        want.extend_from_slice(&[0, 1, 0, 1]);
        want.extend_from_slice(&name);
        want.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 10, 0, 0, 112]);
        assert_eq!(r, want);
        assert_eq!(parse_a_answers(&r, 0xBEEF), Some(vec![Ipv4Addr::new(10, 0, 0, 112)]));
        assert_eq!(parse_a_answers(&r, 0xBEEE), None);
        let empty = build_response(&q, &[]);
        assert_eq!(&empty[6..8], &[0, 0]);
        assert_eq!(parse_a_answers(&empty, 0xBEEF), Some(vec![]));
        let p = parse_query(&win_query("112.0.0.10.in-addr.arpa", rtype::PTR)).unwrap();
        let pr = build_response(&p, &[Answer::Ptr("MacBook-Pro".into())]);
        assert!(pr.ends_with(&[&[0, 13][..], &[11u8], b"MacBook-Pro", &[0]].concat()));
    }

    #[test]
    fn reply_ip_follows_the_interface() {
        let ifs = [
            LocalIf { index: Some(1), ip: Ipv4Addr::LOCALHOST, netmask: Ipv4Addr::new(255, 0, 0, 0), p2p: false },
            LocalIf { index: Some(14), ip: Ipv4Addr::new(10, 5, 0, 2), netmask: Ipv4Addr::new(255, 255, 255, 255), p2p: true },
            LocalIf { index: Some(4), ip: Ipv4Addr::new(10, 0, 0, 112), netmask: Ipv4Addr::new(255, 255, 255, 0), p2p: false },
            LocalIf { index: Some(7), ip: Ipv4Addr::new(192, 168, 2, 5), netmask: Ipv4Addr::new(255, 255, 255, 0), p2p: false },
        ];
        let a = |s: &str| pick_reply_ip(s.parse().unwrap(), &ifs);
        assert_eq!(a("10.0.0.35:51000"), Some(Ipv4Addr::new(10, 0, 0, 112)));
        assert_eq!(a("192.168.2.77:51000"), Some(Ipv4Addr::new(192, 168, 2, 5)));
        assert_eq!(a("[::ffff:192.168.2.77]:51000"), Some(Ipv4Addr::new(192, 168, 2, 5)));
        assert_eq!(a("[fe80::1234%7]:51000"), Some(Ipv4Addr::new(192, 168, 2, 5)));
        assert_eq!(a("[fe80::1234%4]:51000"), Some(Ipv4Addr::new(10, 0, 0, 112)));
        // loopback / unknown sender: the primary LAN address, never the VPN tunnel.
        assert_eq!(a("127.0.0.1:51000"), Some(Ipv4Addr::new(10, 0, 0, 112)));
        assert_eq!(a("172.16.9.9:51000"), Some(Ipv4Addr::new(10, 0, 0, 112)));
        assert_eq!(primary_ipv4(&ifs[..2]), None);
    }

    /// Live: unicast queries to our own responder over loopback (no other
    /// hosts; bound to loopback so the macOS firewall isn't involved).
    #[test]
    fn live_query_over_loopback() {
        let r = Responder::start_on("Test-Mac-Llmnr", Ipv4Addr::LOCALHOST, Some(Ipv6Addr::LOCALHOST), 0).unwrap();
        let port = r.port();
        let target: SocketAddr = (Ipv4Addr::LOCALHOST, port).into();
        let got = query_a("TEST-MAC-LLMNR", target, Duration::from_secs(3)).unwrap().expect("an answer");
        // A loopback sender gets the primary LAN address (what `powermac mwb doctor` shows).
        let want = primary_ipv4(&local_ifs());
        assert_eq!(got, want.into_iter().collect::<Vec<_>>());
        // .local works too; other names get no answer.
        assert!(query_a("test-mac-llmnr.local", target, Duration::from_secs(3)).unwrap().is_some());
        assert_eq!(query_a("SOMEONE-ELSE", target, Duration::from_millis(400)).unwrap(), None);
        let st = r.status();
        assert!(st.running && st.answered >= 2 && st.queries >= 3, "{st:?}");
        assert_eq!(st.last_other.as_ref().map(|(_, n)| n.as_str()), Some("SOMEONE-ELSE"));
        assert!(st.groups_v4.is_empty(), "joined multicast groups while bound to loopback");
        // IPv6 loopback, when the IPv6 socket is up.
        if st.ipv6 {
            let got = query_a("Test-Mac-Llmnr", (Ipv6Addr::LOCALHOST, port).into(), Duration::from_secs(3)).unwrap();
            assert_eq!(got, Some(want.into_iter().collect::<Vec<_>>()));
        }
        r.stop();
        assert!(!r.status().running);
        assert_eq!(query_a("Test-Mac-Llmnr", target, Duration::from_millis(300)).unwrap(), None);
    }

    /// The real thing on all interfaces, queried at this machine's own LAN
    /// address. Ignored by default: the macOS firewall may ask to allow the
    /// (unsigned) test binary. `cargo test -p mwb -- --ignored llmnr`.
    #[test]
    #[ignore]
    fn live_query_on_own_lan_address() {
        let r = Responder::start("Test-Mac-Llmnr", 0).unwrap();
        let Some(lan) = primary_ipv4(&local_ifs()) else { return };
        let got = query_a("Test-Mac-Llmnr", (lan, r.port()).into(), Duration::from_secs(3)).unwrap();
        assert_eq!(got, Some(vec![lan]));
        assert!(!r.status().groups_v4.is_empty());
    }
}
