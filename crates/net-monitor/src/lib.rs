//! Network monitor for PowerMac, after Little Snitch's Network Monitor and
//! Radio Silence's per-app list.
//!
//! Two parts:
//!
//! - **Monitor** (read-only): [`snapshot`] enumerates every TCP and UDP
//!   socket on the system with its owning process (macOS: libproc's
//!   `proc_pidfdinfo`, the source of `lsof -i`; Windows: the IP Helper
//!   owner-pid tables, the source of `netstat -ano`). [`Snapshot::by_app`]
//!   groups them by app (`.app` bundle, so helper processes count as their
//!   app), [`Monitor`] diffs successive snapshots into opened / closed /
//!   state-changed events, and [`watch`] polls. Polling the socket table
//!   sees connections that live at least one interval; a short DNS lookup
//!   can fall between two polls. Nothing is captured or intercepted, and no
//!   packet contents are read.
//! - **Rules** (a model, not enforced): [`rules::RuleSet`] holds per-app
//!   allow / deny / ask rules for remote hosts, domains, addresses,
//!   networks, ports, protocols and directions, evaluated most-specific-wins
//!   like Little Snitch, stored in `net-rules.json`, and importable from
//!   Little Snitch `.lsrules` files. [`rules::RuleSet::audit`] shows what the
//!   rules would do to the live connections. Blocking needs a Network
//!   Extension content filter (macOS) or WFP (Windows); see `rules`.
//!
//! On macOS a normal user sees only their own processes' sockets; run as
//! root (`sudo powermac net list`) to include system daemons. The count of
//! processes that couldn't be read is in [`Snapshot::inaccessible`].

pub mod model;
pub mod monitor;
mod platform;
pub mod rules;

#[cfg(test)]
mod tests;
#[cfg(all(test, target_os = "macos"))]
use platform::macos as platform_test_hooks;

pub use model::{AppConnections, Connection, Direction, ProcessInfo, Protocol, Scope, Snapshot, Socket, TcpState};
pub use monitor::{watch, Event, Monitor};
pub use rules::{Action, AppMatch, RemoteMatch, Rule, RuleSet, Verdict};

/// Every TCP/UDP socket on the system right now.
pub fn snapshot() -> anyhow::Result<Snapshot> {
    platform::snapshot()
}

/// One line for a socket: `TCP 192.168.1.5:52144 → 17.253.144.10:443 ESTABLISHED`.
pub fn describe(s: &Socket) -> String {
    let mut out = format!("{} {}", s.protocol.label(), s.local);
    match s.remote {
        Some(r) => out.push_str(&format!(" → {}", fmt_addr(r))),
        None => out.push_str(if s.protocol == Protocol::Tcp { " (listening)" } else { " (bound)" }),
    }
    if let Some(st) = s.state {
        if st != TcpState::Listen {
            out.push(' ');
            out.push_str(st.label());
        }
    }
    out
}

/// Socket address with IPv4-mapped IPv6 shown as IPv4.
fn fmt_addr(a: std::net::SocketAddr) -> std::net::SocketAddr {
    std::net::SocketAddr::new(model::canonical_ip(a.ip()), a.port())
}
