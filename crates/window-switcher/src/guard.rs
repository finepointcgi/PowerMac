//! Safe take-over of the system switcher (macOS Cmd+Tab), platform
//! independent.
//!
//! On macOS the switcher only receives Cmd+Tab after the Dock's symbolic
//! hotkeys are turned off with `CGSSetSymbolicHotKeyEnabled`. That setting
//! outlives the process, so a crash would leave the user without *any*
//! Cmd+Tab. The [`Guard`] makes that impossible in practice:
//!
//! * the system switcher is only off while the override is wanted **and**
//!   our event tap is healthy — if the tap can't be created or gets
//!   disabled, the system switcher comes back at once;
//! * while it's off, a marker file (`$TMPDIR/…-<pid>.marker`, see
//!   [`marker_file_name`]) records which hotkeys this process turned off;
//!   the next start (or any other PowerMac process) finds markers of dead
//!   processes and turns those hotkeys back on ([`plan_recovery`]);
//! * hotkeys another *live* owner still wants off are left alone, so two
//!   instances don't fight.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Symbolic hotkey ids of the Dock's app switcher: Cmd+Tab, Cmd+Shift+Tab.
pub const HOTKEYS_CMD_TAB: [i32; 2] = [1, 2];
/// Symbolic hotkey ids behind Cmd+`: "Move focus to next window" (27) and
/// the key-above-Tab variant (6) AltTab also turns off.
pub const HOTKEYS_CMD_BACKTICK: [i32; 2] = [6, 27];

/// Marker file name prefix (in `$TMPDIR`).
pub const MARKER_PREFIX: &str = "powermac-window-switcher-cmdtab-";
pub const MARKER_SUFFIX: &str = ".marker";

pub fn marker_file_name(pid: u32) -> String {
    format!("{MARKER_PREFIX}{pid}{MARKER_SUFFIX}")
}

/// What a marker file says: who turned which system hotkeys off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub pid: u32,
    /// Executable path of the owner, to tell a reused pid from the owner.
    pub exe: String,
    pub hotkeys: BTreeSet<i32>,
}

impl Marker {
    pub fn to_text(&self) -> String {
        let ids: Vec<String> = self.hotkeys.iter().map(|i| i.to_string()).collect();
        format!("pid={}\nexe={}\nhotkeys={}\n", self.pid, self.exe, ids.join(","))
    }

    /// Parse a marker; the pid comes from the file name if the text is
    /// damaged. Unknown hotkey lists fall back to Cmd+Tab's (the case that
    /// matters).
    pub fn parse(file_name: &str, text: &str) -> Option<Marker> {
        let name_pid = file_name.strip_prefix(MARKER_PREFIX)?.strip_suffix(MARKER_SUFFIX)?.parse::<u32>().ok();
        let mut pid = None;
        let mut exe = String::new();
        let mut hotkeys: Option<BTreeSet<i32>> = None;
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("pid=") {
                pid = v.trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("exe=") {
                exe = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("hotkeys=") {
                let ids: BTreeSet<i32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                hotkeys = Some(ids);
            }
        }
        let pid = pid.or(name_pid)?;
        let hotkeys = match hotkeys {
            Some(h) if !h.is_empty() => h,
            _ => HOTKEYS_CMD_TAB.into_iter().collect(),
        };
        Some(Marker { pid, exe, hotkeys })
    }
}

/// What to do at start-up about markers left behind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recovery {
    /// Marker files of dead processes (to delete).
    pub stale: Vec<PathBuf>,
    /// Hotkeys to turn back on (a dead owner turned them off and no live
    /// owner wants them off).
    pub reenable: BTreeSet<i32>,
    /// Hotkeys a live process (not us) keeps off.
    pub held_by_others: BTreeSet<i32>,
}

/// Decide which leftovers to clean up. `alive` tells whether a marker's
/// owner is still running (same pid *and* executable).
pub fn plan_recovery(markers: Vec<(PathBuf, Marker)>, own_pid: u32, alive: impl Fn(&Marker) -> bool) -> Recovery {
    let mut r = Recovery::default();
    let mut dead_ids = BTreeSet::new();
    for (path, m) in markers {
        if m.pid == own_pid {
            // Ours from an earlier start in this process (or a pid reused
            // from a dead run): either way nobody else holds it.
            dead_ids.extend(m.hotkeys.iter().copied());
            r.stale.push(path);
        } else if alive(&m) {
            r.held_by_others.extend(m.hotkeys.iter().copied());
        } else {
            dead_ids.extend(m.hotkeys.iter().copied());
            r.stale.push(path);
        }
    }
    r.reenable = dead_ids.difference(&r.held_by_others).copied().collect();
    r
}

/// The side effects the guard needs (the real one talks to the window
/// server and `$TMPDIR`; tests use a fake).
pub trait Backend {
    /// Turn system hotkeys on or off.
    fn set_hotkeys(&mut self, ids: &[i32], enabled: bool);
    /// Record that we keep `ids` off (empty: remove our marker).
    fn write_marker(&mut self, ids: &BTreeSet<i32>);
    /// Hotkeys other live processes keep off.
    fn held_by_others(&mut self) -> BTreeSet<i32>;
}

/// Tracks which system hotkeys this process turned off and keeps that in
/// line with what's wanted and whether input interception works.
#[derive(Debug, Default)]
pub struct Guard {
    off: BTreeSet<i32>,
}

impl Guard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hotkeys this process currently keeps off.
    pub fn disabled(&self) -> &BTreeSet<i32> {
        &self.off
    }

    /// Which hotkeys should be off: only while the override is `want`ed and
    /// the event tap is `healthy`.
    pub fn wanted(want_cmd_tab: bool, want_backtick: bool, healthy: bool) -> BTreeSet<i32> {
        let mut s = BTreeSet::new();
        if healthy && want_cmd_tab {
            s.extend(HOTKEYS_CMD_TAB);
        }
        if healthy && want_backtick {
            s.extend(HOTKEYS_CMD_BACKTICK);
        }
        s
    }

    /// Bring the system hotkeys in line. Returns whether anything changed.
    pub fn sync(&mut self, wanted: BTreeSet<i32>, b: &mut dyn Backend) -> bool {
        if wanted == self.off {
            return false;
        }
        let turn_off: Vec<i32> = wanted.difference(&self.off).copied().collect();
        let turn_on: Vec<i32> = self.off.difference(&wanted).copied().collect();
        // Write the marker *before* turning anything off, so a crash right
        // after still gets cleaned up.
        if !turn_off.is_empty() {
            b.write_marker(&wanted);
            b.set_hotkeys(&turn_off, false);
        }
        if !turn_on.is_empty() {
            let others = b.held_by_others();
            let on: Vec<i32> = turn_on.iter().copied().filter(|i| !others.contains(i)).collect();
            if !on.is_empty() {
                b.set_hotkeys(&on, true);
            }
            if turn_off.is_empty() {
                b.write_marker(&wanted);
            }
        }
        self.off = wanted;
        true
    }

    /// Give everything back (stop, quit, disable).
    pub fn release(&mut self, b: &mut dyn Backend) -> bool {
        self.sync(BTreeSet::new(), b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        system_off: BTreeSet<i32>,
        marker: Option<BTreeSet<i32>>,
        others: BTreeSet<i32>,
        log: Vec<String>,
    }

    impl Backend for Fake {
        fn set_hotkeys(&mut self, ids: &[i32], enabled: bool) {
            for &i in ids {
                if enabled {
                    self.system_off.remove(&i);
                } else {
                    self.system_off.insert(i);
                }
            }
            self.log.push(format!("{} {ids:?}", if enabled { "on" } else { "off" }));
        }
        fn write_marker(&mut self, ids: &BTreeSet<i32>) {
            self.marker = (!ids.is_empty()).then(|| ids.clone());
            self.log.push(format!("marker {ids:?}"));
        }
        fn held_by_others(&mut self) -> BTreeSet<i32> {
            self.others.clone()
        }
    }

    fn set(v: &[i32]) -> BTreeSet<i32> {
        v.iter().copied().collect()
    }

    #[test]
    fn off_only_while_wanted_and_healthy() {
        assert!(Guard::wanted(true, true, false).is_empty(), "no tap → system switcher stays");
        assert_eq!(Guard::wanted(false, true, true), set(&[6, 27]), "Cmd+` on its own");
        assert_eq!(Guard::wanted(true, false, true), set(&[1, 2]));
        assert_eq!(Guard::wanted(true, true, true), set(&[1, 2, 6, 27]));
    }

    #[test]
    fn marker_written_before_disabling_and_removed_after_restoring() {
        let mut g = Guard::new();
        let mut b = Fake::default();
        assert!(g.sync(Guard::wanted(true, false, true), &mut b));
        assert_eq!(b.log, vec!["marker {1, 2}", "off [1, 2]"]);
        assert_eq!(b.system_off, set(&[1, 2]));
        assert!(!g.sync(Guard::wanted(true, false, true), &mut b), "idempotent");
        // Cmd+` added, then the tap dies: everything comes back at once.
        g.sync(Guard::wanted(true, true, true), &mut b);
        assert_eq!(b.system_off, set(&[1, 2, 6, 27]));
        assert_eq!(b.marker, Some(set(&[1, 2, 6, 27])));
        g.sync(Guard::wanted(true, true, false), &mut b);
        assert!(b.system_off.is_empty());
        assert_eq!(b.marker, None);
        // Healthy again → off again; release restores.
        g.sync(Guard::wanted(true, false, true), &mut b);
        assert!(g.release(&mut b));
        assert!(b.system_off.is_empty() && b.marker.is_none());
        assert!(!g.release(&mut b));
    }

    #[test]
    fn restore_leaves_hotkeys_another_live_owner_wants_off() {
        let mut g = Guard::new();
        let mut b = Fake { others: set(&[1, 2]), ..Default::default() };
        g.sync(Guard::wanted(true, true, true), &mut b);
        g.release(&mut b);
        assert_eq!(b.system_off, set(&[1, 2]), "the other process still replaces Cmd+Tab");
        assert!(b.marker.is_none());
    }

    #[test]
    fn marker_roundtrip_and_damaged_markers() {
        let m = Marker { pid: 4242, exe: "/Applications/PowerMac.app/Contents/MacOS/powermac".into(), hotkeys: set(&[1, 2, 27]) };
        let name = marker_file_name(4242);
        assert_eq!(Marker::parse(&name, &m.to_text()), Some(m));
        // Empty file: pid from the name, Cmd+Tab's hotkeys assumed.
        let d = Marker::parse(&name, "").unwrap();
        assert_eq!((d.pid, d.hotkeys), (4242, set(&[1, 2])));
        assert_eq!(Marker::parse("unrelated.txt", "pid=1"), None);
        assert_eq!(Marker::parse(&format!("{MARKER_PREFIX}x{MARKER_SUFFIX}"), "garbage"), None);
    }

    #[test]
    fn crash_recovery_plan() {
        let mk = |pid: u32, exe: &str, ids: &[i32]| {
            (PathBuf::from(marker_file_name(pid)), Marker { pid, exe: exe.into(), hotkeys: set(ids) })
        };
        let alive = |m: &Marker| m.pid == 200 && m.exe == "/bin/live";
        // A crashed run (pid 100) left Cmd+Tab and Cmd+` off.
        let r = plan_recovery(vec![mk(100, "/bin/dead", &[1, 2, 6, 27])], 300, alive);
        assert_eq!(r.reenable, set(&[1, 2, 6, 27]));
        assert_eq!(r.stale, vec![PathBuf::from(marker_file_name(100))]);
        // A live owner (pid 200) still keeps Cmd+Tab off: only Cmd+` comes back.
        let r = plan_recovery(vec![mk(100, "/bin/dead", &[1, 2, 6, 27]), mk(200, "/bin/live", &[1, 2])], 300, alive);
        assert_eq!(r.reenable, set(&[6, 27]));
        assert_eq!(r.held_by_others, set(&[1, 2]));
        assert_eq!(r.stale.len(), 1);
        // Pid reused by another program: treated as dead.
        let r = plan_recovery(vec![mk(200, "/bin/dead", &[1, 2])], 300, alive);
        assert_eq!(r.reenable, set(&[1, 2]));
        // Our own pid's marker (an earlier start in this process) is stale.
        let r = plan_recovery(vec![mk(300, "/bin/me", &[1, 2])], 300, |_| true);
        assert_eq!(r.reenable, set(&[1, 2]));
        // Nothing left behind: nothing to do.
        assert_eq!(plan_recovery(vec![], 300, alive), Recovery::default());
    }
}
