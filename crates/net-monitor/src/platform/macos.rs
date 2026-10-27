//! macOS: libproc, the same source `lsof -i` and `nettop` read.
//!
//! For each pid (`proc_listallpids`) we list its file descriptors
//! (`proc_pidinfo(PROC_PIDLISTFDS)`), and for each socket descriptor read a
//! `struct socket_fdinfo` (`proc_pidfdinfo(PROC_PIDFDSOCKETINFO)`). libc
//! doesn't define that struct, so the fields are read at their offsets in
//! xnu's `sys/proc_info.h` (checked against the SDK with `offsetof`; the
//! struct is 792 bytes on arm64 and x86_64).
//!
//! Without root, the kernel only shows the sockets of the current user's
//! processes; the others are counted in `Snapshot::inaccessible`.

use std::collections::HashSet;
use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::model::{ProcessInfo, Protocol, Snapshot, Socket, TcpState};

const PROC_PIDFDSOCKETINFO: i32 = 3;
pub(crate) const SOCKET_FDINFO_SIZE: usize = 792;

// Offsets inside struct socket_fdinfo (pfi: 24 bytes, then psi).
const PSI: usize = 24;
pub(crate) const OFF_SOI_SO: usize = PSI + 136;
pub(crate) const OFF_SOI_PROTOCOL: usize = PSI + 156;
pub(crate) const OFF_SOI_FAMILY: usize = PSI + 160;
pub(crate) const OFF_SOI_KIND: usize = PSI + 232;
pub(crate) const OFF_PROTO: usize = PSI + 240; // union soi_proto
// struct in_sockinfo, at OFF_PROTO (also the head of tcp_sockinfo).
pub(crate) const OFF_FPORT: usize = OFF_PROTO;
pub(crate) const OFF_LPORT: usize = OFF_PROTO + 4;
pub(crate) const OFF_VFLAG: usize = OFF_PROTO + 24;
pub(crate) const OFF_FADDR: usize = OFF_PROTO + 32;
pub(crate) const OFF_LADDR: usize = OFF_PROTO + 48;
pub(crate) const OFF_TCP_STATE: usize = OFF_PROTO + 80;

pub(crate) const SOCKINFO_IN: i32 = 1;
pub(crate) const SOCKINFO_TCP: i32 = 2;
const INI_IPV4: u8 = 0x1;
const INI_IPV6: u8 = 0x2;

fn rd_i32(b: &[u8], off: usize) -> i32 {
    i32::from_ne_bytes(b[off..off + 4].try_into().unwrap())
}

fn rd_u64(b: &[u8], off: usize) -> u64 {
    u64::from_ne_bytes(b[off..off + 8].try_into().unwrap())
}

/// `insi_fport` / `insi_lport`: an int holding a network-order u16.
fn port(b: &[u8], off: usize) -> u16 {
    u16::from_be(rd_i32(b, off) as u16)
}

/// `union { struct in4in6_addr; struct in6_addr }`: IPv4 sits in the last 4 bytes.
fn addr(b: &[u8], off: usize, v6: bool) -> IpAddr {
    if v6 {
        let o: [u8; 16] = b[off..off + 16].try_into().unwrap();
        IpAddr::V6(Ipv6Addr::from(o))
    } else {
        let o: [u8; 4] = b[off + 12..off + 16].try_into().unwrap();
        IpAddr::V4(Ipv4Addr::from(o))
    }
}

/// One `struct socket_fdinfo` → a TCP/UDP socket (anything else: `None`).
/// Also returns the kernel's socket handle, to drop duplicate descriptors.
pub(crate) fn parse_socket_fdinfo(pid: u32, b: &[u8]) -> Option<(u64, Socket)> {
    if b.len() < SOCKET_FDINFO_SIZE {
        return None;
    }
    let family = rd_i32(b, OFF_SOI_FAMILY);
    if family != libc::AF_INET && family != libc::AF_INET6 {
        return None;
    }
    let kind = rd_i32(b, OFF_SOI_KIND);
    let protocol = match (kind, rd_i32(b, OFF_SOI_PROTOCOL)) {
        (SOCKINFO_TCP, _) => Protocol::Tcp,
        (SOCKINFO_IN, p) if p == libc::IPPROTO_UDP => Protocol::Udp,
        (SOCKINFO_IN, p) if p == libc::IPPROTO_TCP => Protocol::Tcp,
        _ => return None,
    };
    let vflag = b[OFF_VFLAG];
    // A dual-stack socket talking IPv4 has INI_IPV4 set and an in4in6 address.
    let v6 = if vflag & INI_IPV4 != 0 {
        false
    } else if vflag & INI_IPV6 != 0 {
        true
    } else {
        family == libc::AF_INET6
    };
    let local = SocketAddr::new(addr(b, OFF_LADDR, v6), port(b, OFF_LPORT));
    let fport = port(b, OFF_FPORT);
    let faddr = addr(b, OFF_FADDR, v6);
    let remote = (fport != 0 || !faddr.is_unspecified()).then(|| SocketAddr::new(faddr, fport));
    let state = if kind == SOCKINFO_TCP { TcpState::from_xnu(rd_i32(b, OFF_TCP_STATE)) } else { None };
    Some((rd_u64(b, OFF_SOI_SO), Socket { pid, protocol, local, remote, state }))
}

fn all_pids() -> Result<Vec<i32>> {
    // SAFETY: a null buffer asks for the count.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        bail!("proc_listallpids failed: {}", std::io::Error::last_os_error());
    }
    let mut pids = vec![0i32; n as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<i32>()) as i32;
    // SAFETY: `pids` has room for `bytes` bytes.
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr() as *mut c_void, bytes) };
    if n <= 0 {
        bail!("proc_listallpids failed: {}", std::io::Error::last_os_error());
    }
    pids.truncate(n as usize);
    pids.retain(|&p| p > 0);
    Ok(pids)
}

/// The process's descriptors, or `None` when we may not look.
fn fds(pid: i32) -> Option<Vec<libc::proc_fdinfo>> {
    // SAFETY: a null buffer asks for the size.
    let need = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return None;
    }
    let sz = std::mem::size_of::<libc::proc_fdinfo>();
    let mut v: Vec<libc::proc_fdinfo> = Vec::with_capacity(need as usize / sz + 32);
    let cap = (v.capacity() * sz) as i32;
    // SAFETY: the buffer holds `cap` bytes; we only keep what the kernel wrote.
    let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, v.as_mut_ptr() as *mut c_void, cap) };
    if got <= 0 {
        return None;
    }
    unsafe { v.set_len(got as usize / sz) };
    Some(v)
}

fn process_info(pid: i32) -> ProcessInfo {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: buffer of the given size.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    let path = (n > 0).then(|| PathBuf::from(String::from_utf8_lossy(&buf[..n as usize]).into_owned()));
    let mut nb = [0u8; 256];
    // SAFETY: buffer of the given size.
    let n = unsafe { libc::proc_name(pid, nb.as_mut_ptr() as *mut c_void, nb.len() as u32) };
    let name = if n > 0 {
        String::from_utf8_lossy(&nb[..n as usize]).into_owned()
    } else {
        path.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| format!("pid {pid}"))
    };
    // proc_name is cut at 32 characters (MAXCOMLEN); the path has the full name.
    let name = match path.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().into_owned()) {
        Some(full) if full.starts_with(&name) => full,
        _ => name,
    };
    ProcessInfo { pid: pid as u32, name, path }
}

pub fn snapshot() -> Result<Snapshot> {
    let mut processes = Vec::new();
    let mut sockets = Vec::new();
    let mut inaccessible = 0u32;
    // `socket_fdinfo` contains u64s: keep the buffer 8-byte aligned.
    let mut buf = vec![0u64; SOCKET_FDINFO_SIZE / 8];
    for pid in all_pids()? {
        let Some(fds) = fds(pid) else {
            inaccessible += 1;
            continue;
        };
        let mut seen = HashSet::new();
        let before = sockets.len();
        for fd in fds.iter().filter(|f| f.proc_fdtype as i32 == libc::PROX_FDTYPE_SOCKET) {
            // SAFETY: `buf` is SOCKET_FDINFO_SIZE bytes.
            let n = unsafe { libc::proc_pidfdinfo(pid, fd.proc_fd, PROC_PIDFDSOCKETINFO, buf.as_mut_ptr() as *mut c_void, SOCKET_FDINFO_SIZE as i32) };
            if n as usize != SOCKET_FDINFO_SIZE {
                continue; // closed meanwhile
            }
            // SAFETY: plain bytes view of the u64 buffer.
            let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, SOCKET_FDINFO_SIZE) };
            if let Some((so, s)) = parse_socket_fdinfo(pid as u32, bytes) {
                if seen.insert(so) {
                    sockets.push(s);
                }
            }
        }
        if sockets.len() > before {
            processes.push(process_info(pid));
        }
    }
    Ok(Snapshot::new(processes, sockets, inaccessible))
}
