//! The live part: compare successive snapshots and report which connections
//! opened, closed or changed state, plus a polling loop.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::model::{Direction, ProcessInfo, Snapshot, Socket, SocketKey, TcpState};

/// What changed between two snapshots.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "event")]
pub enum Event {
    Opened { socket: Socket, direction: Direction, process: ProcessInfo },
    Closed { socket: Socket, process: ProcessInfo, open_for_ms: u64 },
    StateChanged { socket: Socket, process: ProcessInfo, from: Option<TcpState>, to: Option<TcpState> },
}

impl Event {
    pub fn socket(&self) -> &Socket {
        match self {
            Event::Opened { socket, .. } | Event::Closed { socket, .. } | Event::StateChanged { socket, .. } => socket,
        }
    }
}

struct Seen {
    socket: Socket,
    process: ProcessInfo,
    first_seen_ms: u64,
}

/// Keeps the previous snapshot's sockets and turns each new snapshot into
/// events. The first snapshot only primes it (no events), so starting the
/// monitor doesn't report every existing connection as new.
#[derive(Default)]
pub struct Monitor {
    seen: HashMap<SocketKey, Seen>,
    primed: bool,
    /// Report listeners and unconnected UDP sockets too.
    pub include_listeners: bool,
}

impl Monitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// A monitor that also reports listeners and unconnected UDP sockets.
    pub fn with_listeners(include_listeners: bool) -> Self {
        Monitor { include_listeners, ..Self::default() }
    }

    /// Feed the next snapshot; returns what changed since the last one.
    pub fn update(&mut self, snap: &Snapshot) -> Vec<Event> {
        let directions = snap.directions();
        let procs: HashMap<u32, &ProcessInfo> = snap.processes.iter().map(|p| (p.pid, p)).collect();
        let mut next: HashMap<SocketKey, Seen> = HashMap::with_capacity(snap.sockets.len());
        let mut events = Vec::new();
        for (s, dir) in snap.sockets.iter().zip(directions) {
            if !self.include_listeners && s.is_listener() {
                continue;
            }
            let process = procs.get(&s.pid).map(|p| (*p).clone()).unwrap_or_else(|| ProcessInfo { pid: s.pid, name: format!("pid {}", s.pid), path: None });
            let key = s.key();
            let first_seen_ms = match self.seen.remove(&key) {
                Some(old) => {
                    if old.socket.state != s.state && self.primed {
                        events.push(Event::StateChanged { socket: s.clone(), process: process.clone(), from: old.socket.state, to: s.state });
                    }
                    old.first_seen_ms
                }
                None => {
                    if self.primed {
                        events.push(Event::Opened { socket: s.clone(), direction: dir, process: process.clone() });
                    }
                    snap.taken_at_ms
                }
            };
            next.insert(key, Seen { socket: s.clone(), process, first_seen_ms });
        }
        if self.primed {
            let mut closed: Vec<Seen> = self.seen.drain().map(|(_, v)| v).collect();
            closed.sort_by_key(|s| s.socket.key());
            for old in closed {
                events.push(Event::Closed { open_for_ms: snap.taken_at_ms.saturating_sub(old.first_seen_ms), socket: old.socket, process: old.process });
            }
        }
        self.seen = next;
        self.primed = true;
        events
    }

    /// Number of connections currently tracked.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Poll the system every `interval` until `stop` is set, calling `on_tick`
/// with each snapshot and the events since the previous one. Errors from a
/// single snapshot are logged and skipped.
pub fn watch(interval: Duration, include_listeners: bool, stop: Arc<AtomicBool>, mut on_tick: impl FnMut(&Snapshot, &[Event])) {
    let mut m = Monitor::with_listeners(include_listeners);
    while !stop.load(Ordering::Relaxed) {
        let t = Instant::now();
        match crate::snapshot() {
            Ok(snap) => {
                let ev = m.update(&snap);
                on_tick(&snap, &ev);
            }
            Err(e) => log::warn!("net-monitor: snapshot failed: {e:#}"),
        }
        let left = interval.saturating_sub(t.elapsed());
        // Sleep in slices so `stop` is noticed quickly.
        let until = Instant::now() + left;
        while !stop.load(Ordering::Relaxed) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50).min(until.saturating_duration_since(Instant::now())));
        }
    }
}
