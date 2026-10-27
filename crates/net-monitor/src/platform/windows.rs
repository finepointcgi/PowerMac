//! Windows: the IP Helper owner-pid tables (`GetExtendedTcpTable` /
//! `GetExtendedUdpTable`, IPv4 and IPv6), what `netstat -ano` shows, and
//! `QueryFullProcessImageNameW` for each owner. Every user may read the
//! tables; only the image path of some system processes needs elevation.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, MIB_UDP6ROW_OWNER_PID, MIB_UDPROW_OWNER_PID,
    TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

use crate::model::{ProcessInfo, Protocol, Snapshot, Socket, TcpState};

/// Call a GetExtended*Table function until the buffer is big enough. The
/// buffer is u64s so the row structs are aligned.
fn table(f: impl Fn(*mut core::ffi::c_void, *mut u32) -> u32) -> Result<Vec<u64>> {
    let mut size = 0u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..8 {
        let r = f(if buf.is_empty() { std::ptr::null_mut() } else { buf.as_mut_ptr() as *mut _ }, &mut size);
        if r == NO_ERROR {
            return Ok(buf);
        }
        if r != ERROR_INSUFFICIENT_BUFFER {
            bail!("IP Helper table query failed (error {r})");
        }
        buf = vec![0u64; (size as usize).div_ceil(8) + 64];
        size = (buf.len() * 8) as u32;
    }
    bail!("the connection table kept growing")
}

/// `dwNumEntries` followed by the rows.
fn rows<T: Copy>(buf: &[u64]) -> Vec<T> {
    if buf.is_empty() {
        return Vec::new();
    }
    let base = buf.as_ptr() as *const u8;
    // SAFETY: the table is a u32 count followed by `count` rows of T, aligned
    // as the C struct; we stay within the buffer.
    unsafe {
        let n = *(base as *const u32) as usize;
        let first = base.add(std::mem::align_of::<T>().max(4)) as *const T;
        let max = (buf.len() * 8).saturating_sub(std::mem::align_of::<T>().max(4)) / std::mem::size_of::<T>();
        (0..n.min(max)).map(|i| *first.add(i)).collect()
    }
}

fn port(p: u32) -> u16 {
    u16::from_be(p as u16)
}

fn v4(a: u32) -> IpAddr {
    IpAddr::V4(Ipv4Addr::from(a.to_ne_bytes()))
}

fn v6(a: [u8; 16]) -> IpAddr {
    IpAddr::V6(Ipv6Addr::from(a))
}

fn remote(ip: IpAddr, port: u16) -> Option<SocketAddr> {
    (port != 0 || !ip.is_unspecified()).then(|| SocketAddr::new(ip, port))
}

fn image_path(pid: u32) -> Option<PathBuf> {
    // SAFETY: plain Win32 calls; the handle is closed below.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

pub fn snapshot() -> Result<Snapshot> {
    let mut sockets = Vec::new();
    // SAFETY (all four): the closures pass our buffer and its size through.
    let t4 = table(|b, s| unsafe { GetExtendedTcpTable(b, s, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_ALL, 0) })?;
    for r in rows::<MIB_TCPROW_OWNER_PID>(&t4) {
        let state = TcpState::from_mib(r.dwState);
        let rem = if state == Some(TcpState::Listen) { None } else { remote(v4(r.dwRemoteAddr), port(r.dwRemotePort)) };
        sockets.push(Socket { pid: r.dwOwningPid, protocol: Protocol::Tcp, local: SocketAddr::new(v4(r.dwLocalAddr), port(r.dwLocalPort)), remote: rem, state });
    }
    let t6 = table(|b, s| unsafe { GetExtendedTcpTable(b, s, 0, AF_INET6 as u32, TCP_TABLE_OWNER_PID_ALL, 0) })?;
    for r in rows::<MIB_TCP6ROW_OWNER_PID>(&t6) {
        let state = TcpState::from_mib(r.dwState);
        let rem = if state == Some(TcpState::Listen) { None } else { remote(v6(r.ucRemoteAddr), port(r.dwRemotePort)) };
        sockets.push(Socket { pid: r.dwOwningPid, protocol: Protocol::Tcp, local: SocketAddr::new(v6(r.ucLocalAddr), port(r.dwLocalPort)), remote: rem, state });
    }
    // The UDP owner tables have no remote end (UDP sockets are mostly unconnected).
    let u4 = table(|b, s| unsafe { GetExtendedUdpTable(b, s, 0, AF_INET as u32, UDP_TABLE_OWNER_PID, 0) })?;
    for r in rows::<MIB_UDPROW_OWNER_PID>(&u4) {
        sockets.push(Socket { pid: r.dwOwningPid, protocol: Protocol::Udp, local: SocketAddr::new(v4(r.dwLocalAddr), port(r.dwLocalPort)), remote: None, state: None });
    }
    let u6 = table(|b, s| unsafe { GetExtendedUdpTable(b, s, 0, AF_INET6 as u32, UDP_TABLE_OWNER_PID, 0) })?;
    for r in rows::<MIB_UDP6ROW_OWNER_PID>(&u6) {
        sockets.push(Socket { pid: r.dwOwningPid, protocol: Protocol::Udp, local: SocketAddr::new(v6(r.ucLocalAddr), port(r.dwLocalPort)), remote: None, state: None });
    }

    let pids: HashSet<u32> = sockets.iter().map(|s| s.pid).collect();
    let mut paths: HashMap<u32, Option<PathBuf>> = HashMap::new();
    let mut inaccessible = 0;
    for &pid in &pids {
        let p = image_path(pid);
        if p.is_none() && pid > 4 {
            inaccessible += 1;
        }
        paths.insert(pid, p);
    }
    let mut processes: Vec<ProcessInfo> = paths
        .into_iter()
        .map(|(pid, path)| {
            let name = match (&path, pid) {
                (Some(p), _) => p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
                (None, 0) => "System Idle Process".into(),
                (None, 4) => "System".into(),
                (None, _) => format!("pid {pid}"),
            };
            ProcessInfo { pid, name, path }
        })
        .collect();
    processes.sort_by_key(|p| p.pid);
    Ok(Snapshot::new(processes, sockets, inaccessible))
}
