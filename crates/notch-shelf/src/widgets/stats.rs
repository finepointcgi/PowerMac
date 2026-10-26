//! System stats math: CPU load from tick counters, network rates from byte
//! counters (with 32-bit wraparound), a fixed-size history for sparklines.

use crate::geometry::{Point, Rect};

/// Samples kept for sparklines (one per second).
pub const HISTORY: usize = 40;

/// Cumulative CPU ticks (all cores).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuTicks {
    pub user: u64,
    pub system: u64,
    pub idle: u64,
    pub nice: u64,
}

impl CpuTicks {
    fn busy(&self) -> u64 {
        self.user + self.system + self.nice
    }
    fn total(&self) -> u64 {
        self.busy() + self.idle
    }
}

/// CPU busy fraction (0..1) between two samples; `None` without progress.
pub fn cpu_load(prev: &CpuTicks, cur: &CpuTicks) -> Option<f64> {
    let total = cur.total().checked_sub(prev.total())?;
    let busy = cur.busy().checked_sub(prev.busy())?;
    (total > 0).then(|| (busy as f64 / total as f64).clamp(0.0, 1.0))
}

/// Interface byte counters; `wrap` is the counter width (2^32 for the
/// `if_data` counters macOS reports through getifaddrs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetCounters {
    pub rx: u64,
    pub tx: u64,
}

/// Difference of a counter that wraps at `modulo` (0 = no wrapping).
pub fn counter_delta(prev: u64, cur: u64, modulo: u64) -> u64 {
    if cur >= prev {
        cur - prev
    } else if modulo > 0 && prev < modulo {
        (modulo - prev) + cur
    } else {
        // Counter reset (interface went away): treat as no traffic.
        0
    }
}

/// Bytes per second (rx, tx) between two samples `dt` seconds apart. Each
/// interface is diffed on its own (so wraps are per counter).
pub fn net_rate(prev: &[(String, NetCounters)], cur: &[(String, NetCounters)], dt: f64, modulo: u64) -> (f64, f64) {
    if dt <= 0.0 {
        return (0.0, 0.0);
    }
    let mut rx = 0u64;
    let mut tx = 0u64;
    for (name, c) in cur {
        if let Some((_, p)) = prev.iter().find(|(n, _)| n == name) {
            rx += counter_delta(p.rx, c.rx, modulo);
            tx += counter_delta(p.tx, c.tx, modulo);
        }
    }
    (rx as f64 / dt, tx as f64 / dt)
}

/// Interfaces that carry real traffic (not loopback, tunnels that would
/// double count VPN traffic, or Apple-internal links).
pub fn counts_interface(name: &str) -> bool {
    const SKIP: [&str; 10] = ["lo", "utun", "ipsec", "gif", "stf", "awdl", "llw", "bridge", "anpi", "ap"];
    !SKIP.iter().any(|p| name.starts_with(p) && name[p.len()..].chars().all(|c| c.is_ascii_digit()))
}

/// Fixed-capacity history, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    pub values: Vec<f64>,
}

impl History {
    pub fn push(&mut self, v: f64) {
        if self.values.len() >= HISTORY {
            self.values.remove(0);
        }
        self.values.push(v);
    }
    pub fn last(&self) -> Option<f64> {
        self.values.last().copied()
    }
    pub fn max(&self) -> f64 {
        self.values.iter().copied().fold(0.0, f64::max)
    }
}

/// Polyline points for a sparkline in `r` (top-left coords), spread over
/// the full width (the newest sample at the right edge); `scale_max` is the
/// value drawn at the top (values above it are clipped).
pub fn sparkline(values: &[f64], r: Rect, scale_max: f64) -> Vec<Point> {
    if values.len() < 2 || r.w <= 0.0 {
        return vec![];
    }
    let n = values.len();
    let step = r.w / (n - 1) as f64;
    let x0 = r.x;
    let m = if scale_max > 0.0 { scale_max } else { 1.0 };
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let f = (v / m).clamp(0.0, 1.0);
            Point::new(x0 + step * i as f64, r.bottom() - f * r.h)
        })
        .collect()
}

/// A "nice" top for a rate sparkline: at least 64 KB/s, rounded up to
/// 1/2/5 × 10^n.
pub fn nice_max(v: f64) -> f64 {
    let v = v.max(64.0 * 1024.0);
    let p = 10f64.powf(v.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if v <= m * p {
            return m * p;
        }
    }
    10.0 * p
}

/// "0 KB/s", "840 KB/s", "1.2 MB/s", "12 MB/s".
pub fn format_rate(bps: f64) -> String {
    let kb = bps / 1024.0;
    if kb < 1000.0 {
        format!("{:.0} KB/s", kb.max(0.0))
    } else {
        let mb = kb / 1024.0;
        if mb < 10.0 {
            format!("{mb:.1} MB/s")
        } else if mb < 1000.0 {
            format!("{mb:.0} MB/s")
        } else {
            format!("{:.1} GB/s", mb / 1024.0)
        }
    }
}

/// Memory pressure as macOS reports it (`kern.memorystatus_vm_pressure_level`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pressure {
    #[default]
    Normal,
    Warning,
    Critical,
}

impl Pressure {
    pub fn from_level(l: i32) -> Pressure {
        match l {
            4 => Pressure::Critical,
            2 => Pressure::Warning,
            _ => Pressure::Normal,
        }
    }
}

/// What the stats widget shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsView {
    /// CPU load 0..1.
    pub cpu: History,
    /// Memory in use 0..1.
    pub mem: History,
    pub pressure: Pressure,
    /// Bytes/s.
    pub rx: History,
    pub tx: History,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_load_from_ticks() {
        let a = CpuTicks { user: 100, system: 50, idle: 850, nice: 0 };
        let b = CpuTicks { user: 160, system: 70, idle: 1010, nice: 10 };
        assert!((cpu_load(&a, &b).unwrap() - 90.0 / 250.0).abs() < 1e-12);
        assert_eq!(cpu_load(&a, &a), None);
        // Counters going backwards (e.g. a reset) give no reading.
        assert_eq!(cpu_load(&b, &a), None);
    }

    #[test]
    fn net_rates_handle_wraparound_and_new_interfaces() {
        let m = 1u64 << 32;
        assert_eq!(counter_delta(10, 30, m), 20);
        assert_eq!(counter_delta(m - 100, 50, m), 150);
        assert_eq!(counter_delta(500, 10, 0), 0);
        let prev = vec![("en0".to_string(), NetCounters { rx: m - 1000, tx: 0 })];
        let cur = vec![("en0".to_string(), NetCounters { rx: 1048, tx: 4096 }), ("en5".to_string(), NetCounters { rx: 1 << 30, tx: 0 })];
        let (rx, tx) = net_rate(&prev, &cur, 2.0, m);
        assert_eq!(rx, 1024.0);
        assert_eq!(tx, 2048.0);
        assert_eq!(net_rate(&prev, &cur, 0.0, m), (0.0, 0.0));
    }

    #[test]
    fn interface_filter() {
        for n in ["en0", "en12", "pdp_ip0", "bond0", "apple0x"] {
            assert!(counts_interface(n), "{n}");
        }
        for n in ["lo0", "utun3", "awdl0", "llw0", "bridge0", "gif0", "stf0", "anpi1", "ap1", "ipsec0"] {
            assert!(!counts_interface(n), "{n}");
        }
    }

    #[test]
    fn history_and_sparkline() {
        let mut h = History::default();
        for i in 0..(HISTORY + 5) {
            h.push(i as f64);
        }
        assert_eq!(h.values.len(), HISTORY);
        assert_eq!(h.values[0], 5.0);
        assert_eq!(h.last(), Some((HISTORY + 4) as f64));
        let r = Rect::new(10.0, 20.0, 78.0, 10.0);
        let pts = sparkline(&[0.0, 0.5, 2.0], r, 1.0);
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[2], Point::new(88.0, 20.0)); // clipped at the top, right edge
        assert_eq!(pts[1].y, 25.0);
        assert_eq!(pts[0].y, 30.0);
        assert!((pts[1].x - 49.0).abs() < 1e-9); // spread over the width
        assert_eq!(pts[0].x, 10.0);
        let full: Vec<f64> = (0..HISTORY).map(|_| 0.0).collect();
        let pts = sparkline(&full, r, 1.0);
        assert!((pts[0].x - r.x).abs() < 1e-9);
        assert!(sparkline(&[1.0], r, 1.0).is_empty());
    }

    #[test]
    fn rate_formatting_and_scale() {
        assert_eq!(format_rate(0.0), "0 KB/s");
        assert_eq!(format_rate(840.0 * 1024.0), "840 KB/s");
        assert_eq!(format_rate(1.25 * 1024.0 * 1024.0), "1.2 MB/s");
        assert_eq!(format_rate(12.0 * 1024.0 * 1024.0), "12 MB/s");
        assert_eq!(format_rate(2.5 * 1024.0 * 1024.0 * 1024.0), "2.5 GB/s");
        assert_eq!(nice_max(0.0), 100_000.0);
        assert_eq!(nice_max(150_000.0), 200_000.0);
        assert_eq!(nice_max(3_000_000.0), 5_000_000.0);
        assert_eq!(Pressure::from_level(4), Pressure::Critical);
        assert_eq!(Pressure::from_level(1), Pressure::Normal);
    }
}
