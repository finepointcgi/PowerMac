//! Menu bar manager (Ice / Bartender style): too many menu bar icons, and
//! on notched MacBooks some of them end up behind the camera housing.
//!
//! # How it works (macOS)
//!
//! PowerMac adds three status items of its own. Left to right:
//!
//! ```text
//! [always-hidden items] ┆ [hidden items] │ ‹  [visible items] [system items]
//!                       │                │ └ chevron (click to show / hide)
//!                       │                └ hidden-section separator
//!                       └ always-hidden separator (optional)
//! ```
//!
//! Hiding a section makes its separator very long, which pushes every
//! status item to its left out of the menu bar; showing it makes the
//! separator short again (the Ice technique). The user decides which icons
//! go where by Cmd-dragging them across the separators, as macOS allows for
//! any status item; [`Handle::move_item`] does the same programmatically by
//! synthesizing a Cmd-drag. macOS remembers where our items are through
//! `NSStatusItem.autosaveName`, so the arrangement survives restarts.
//!
//! On macOS 26 and later the menu bar takes items that don't fit out of the
//! bar instead of pushing them off-screen, so a 10 000 pt separator would
//! hide *itself*. There the separator is made as long as still fits (found
//! by bisection on the menu bar's layout feedback), which has the same
//! effect on the items left of it. Also, status items are no longer
//! separate windows there, so listing other apps' items uses the
//! Accessibility API (`AXExtrasMenuBar`) instead of `CGWindowListCopyWindowInfo`.
//!
//! Behaviors: click the chevron to show/hide the hidden section,
//! Option-click (or right-click) for the always-hidden one too; optional
//! show-on-hover over the empty part of the menu bar; auto-rehide after N
//! seconds without menu bar activity and/or when another app becomes
//! active; an optional floating "bar" under the menu bar that shows the
//! hidden items (as images) instead of expanding in place, used always or
//! only when expanding in place would put items behind the notch.
//!
//! # Windows
//!
//! The notification area already has an overflow flyout (the ^ chevron)
//! with drag-to-arrange, so the Windows backend is a no-op that compiles:
//! [`MenuBarManager::start`] succeeds, [`Handle::items`] is empty.
//!
//! All AppKit work runs on the main thread (main dispatch queue); the host
//! must run `NSApplication` on the main thread, as the PowerMac tray does.

pub mod logic;
mod platform;

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub use logic::{Expansion, Rect, Section};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Opt-in: off by default.
    pub enabled: bool,
    /// Hide the shown section again after `rehide_delay_secs` without
    /// menu bar activity (pointer in the menu bar or the bar, open menus).
    pub auto_rehide: bool,
    pub rehide_delay_secs: u64,
    /// Also hide as soon as another app becomes active.
    pub rehide_on_focus_change: bool,
    /// Show the hidden section when the pointer rests on the empty part of
    /// the menu bar (and hide it once the pointer leaves the menu bar).
    pub show_on_hover: bool,
    pub hover_delay_ms: u64,
    /// Show hidden items in a floating bar below the menu bar instead of
    /// expanding in place (needs Screen Recording for other apps' item
    /// images on macOS 15 and older; app icons are shown otherwise).
    pub use_floating_bar: bool,
    /// When expanding in place would put items behind the notch, use the
    /// floating bar instead, so nothing is lost.
    pub notch_fallback_to_bar: bool,
    /// Add the second separator for an always-hidden section.
    pub always_hidden_section: bool,
    /// Animate the separator when showing / hiding.
    pub animate: bool,
    /// Draw the separators (thin lines) while their sections are shown.
    pub show_separators: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: false,
            auto_rehide: true,
            rehide_delay_secs: 10,
            rehide_on_focus_change: true,
            show_on_hover: false,
            hover_delay_ms: 300,
            use_floating_bar: false,
            notch_fallback_to_bar: true,
            always_hidden_section: true,
            animate: true,
            show_separators: true,
        }
    }
}

/// One status item in the menu bar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuBarItem {
    /// Stable id across scans (`bundle id or owner` + `/label` or `#index`).
    pub id: String,
    /// Owning app's name.
    pub owner: String,
    pub bundle_id: Option<String>,
    pub pid: i32,
    /// The item's title / accessibility description when it has one.
    pub title: Option<String>,
    /// Global top-left points. For items in a hidden section this is
    /// off-screen (or, on macOS 26+, wherever the menu bar parked it).
    pub frame: Rect,
    pub section: Section,
    /// The item's window (macOS 15 and older; `None` on macOS 26+).
    pub window_id: Option<u32>,
    /// One of this process's own status items (e.g. the PowerMac tray icon).
    pub own: bool,
    /// Apple's own items (clock, Control Center, …), which can't always be
    /// moved.
    pub system: bool,
}

/// Where [`Handle::move_item`] puts an item.
#[derive(Clone, Debug, PartialEq)]
pub enum MoveTarget {
    /// The edge of a section next to its separator: `Visible` = just right of
    /// the chevron, `Hidden` = just left of the hidden separator,
    /// `AlwaysHidden` = just left of the always-hidden separator.
    Section(Section),
    /// Next to another item (by [`MenuBarItem::id`]).
    LeftOf(String),
    RightOf(String),
}

/// Permissions that the optional parts need.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    /// Listing other apps' items on macOS 26+, moving items, forwarding
    /// clicks from the floating bar.
    pub accessibility: bool,
    /// Images of other apps' items in the floating bar (macOS 15 and older).
    pub screen_recording: bool,
}

pub fn permissions() -> Permissions {
    platform::permissions()
}

/// Items behind the notch right now (status items that should be shown but
/// overlap the camera housing or were pushed left of it), without a running
/// manager.
pub fn items_hidden_by_notch() -> Vec<MenuBarItem> {
    let snap = platform::snapshot_now();
    hidden_by_notch(&snap, &collect_items(&snap, &HashMap::new()))
}

/// A status item the menu bar really shows (see [`visible_status_items`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibleItem {
    /// Left edge (first point where the hit test found it).
    pub x: f64,
    pub pid: i32,
    pub title: Option<String>,
}

/// Diagnostics: which status items the menu bar really shows, left to
/// right, found by hit-testing along it (Accessibility). On macOS 26+ this is
/// the ground truth (status item frames don't tell whether the menu bar moved
/// an item into its overflow). Empty on the main thread (it would wait on
/// itself), without Accessibility, and on Windows. Takes about a second.
pub fn visible_status_items() -> Vec<VisibleItem> {
    let snap = platform::snapshot_now();
    let Some(screen) = snap.screen else { return vec![] };
    platform::scan_visible(screen, snap.menu_bar_h).into_iter().map(|(x, pid, title)| VisibleItem { x, pid, title }).collect()
}

/// Forget where our separators were (they go back to the left end of the
/// status items, i.e. nothing hidden, next time).
pub fn reset_positions() {
    platform::reset_positions();
}

// ---------------------------------------------------------------------------

/// Requests from the [`Handle`] to the main thread.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum Request {
    Toggle { all: bool },
    Show(Expansion),
    Hide,
    ShowBar { all: bool },
    /// Show the item's section in place for a moment and press it.
    Press(String),
    /// Refresh the snapshot (own item frames).
    Refresh,
    /// Suspend auto-rehide / focus / hover hiding (while moving items).
    Hold(bool),
}

/// Our own status item as seen on the main thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OwnItem {
    pub title: Option<String>,
    pub frame: Rect,
    pub index: usize,
}

/// What the main thread last saw.
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub gen: u64,
    pub active: bool,
    pub expansion: Expansion,
    /// The section is being shown through the floating bar.
    pub bar_open: bool,
    /// macOS 26+ "longest length that fits" mode.
    pub fitted_mode: bool,
    /// Animating / searching: frames are in flux.
    pub busy: bool,
    pub toggle: Option<Rect>,
    pub hidden: Option<Rect>,
    pub always_hidden: Option<Rect>,
    /// Whether `always_hidden`'s frame is real right now.
    pub always_hidden_reliable: bool,
    pub screen: Option<Rect>,
    pub menu_bar_h: f64,
    pub notch: Option<Rect>,
    pub own_items: Vec<OwnItem>,
    pub own_name: String,
    pub own_bundle: Option<String>,
    pub own_pid: i32,
    pub layout_issue: Option<String>,
}

pub(crate) struct State {
    pub config: Config,
    pub config_gen: u64,
    pub requests: Vec<Request>,
    pub stopped: bool,
    pub snapshot: Snapshot,
    /// Last known section per item id, for when the frames can't tell
    /// (macOS 26+ with everything hidden).
    pub section_cache: HashMap<String, Section>,
    /// Last listing, used to decide on the floating bar at show time.
    pub last_items: Vec<MenuBarItem>,
}

pub(crate) struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn publish(&self, snap: Snapshot) {
        let mut st = self.lock();
        let gen = st.snapshot.gen + 1;
        st.snapshot = Snapshot { gen, ..snap };
        self.changed.notify_all();
    }

    /// Wait until the snapshot generation passes `gen` and `pred` holds.
    pub fn wait_for(&self, gen: u64, timeout: Duration, pred: impl Fn(&Snapshot) -> bool) -> Option<Snapshot> {
        let deadline = Instant::now() + timeout;
        let mut st = self.lock();
        loop {
            if st.snapshot.gen > gen && pred(&st.snapshot) {
                return Some(st.snapshot.clone());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            st = self.changed.wait_timeout(st, left).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
        }
    }
}

/// Raw listing entry from the platform.
#[derive(Clone, Debug, Default)]
pub(crate) struct RawItem {
    pub owner: String,
    pub bundle_id: Option<String>,
    pub pid: i32,
    pub title: Option<String>,
    /// What the item calls itself (identifier / title / description).
    pub label: Option<String>,
    pub index: usize,
    pub frame: Rect,
    pub window_id: Option<u32>,
    pub own: bool,
    pub system: bool,
}

fn collect_items(snap: &Snapshot, cache: &HashMap<String, Section>) -> Vec<MenuBarItem> {
    let mut raw = platform::list_other_items();
    for o in &snap.own_items {
        raw.push(RawItem {
            owner: snap.own_name.clone(),
            bundle_id: snap.own_bundle.clone(),
            pid: snap.own_pid,
            title: o.title.clone(),
            label: o.title.clone(),
            index: o.index,
            frame: o.frame,
            window_id: None,
            own: true,
            system: false,
        });
    }
    let mut v: Vec<MenuBarItem> = raw
        .into_iter()
        .map(|r| {
            let id = logic::item_id(&r.owner, r.bundle_id.as_deref(), r.label.as_deref(), r.index);
            let section = match (snap.active, snap.hidden) {
                // macOS 26+ shows its own overflow chevron at the left end of
                // the status items when some don't fit: it's always visible.
                _ if r.system && r.label.as_deref().is_some_and(|l| l.contains("overflow")) => Section::Visible,
                (true, Some(h)) => {
                    let ah = snap.always_hidden.filter(|_| snap.always_hidden_reliable).map(|a| a.right());
                    logic::assign_section(&r.frame, h.right(), ah, snap.always_hidden.is_some())
                        .unwrap_or_else(|| cache.get(&id).copied().filter(|s| *s != Section::Visible).unwrap_or(Section::Hidden))
                }
                _ => Section::Visible,
            };
            MenuBarItem {
                id,
                owner: r.owner,
                bundle_id: r.bundle_id,
                pid: r.pid,
                title: r.title,
                frame: r.frame,
                section,
                window_id: r.window_id,
                own: r.own,
                system: r.system,
            }
        })
        .collect();
    v.sort_by(|a, b| a.frame.x.total_cmp(&b.frame.x));
    v
}

/// List the items and remember what was learned (sections while the
/// frames can tell them apart; the listing for show-time decisions).
pub(crate) fn refresh_items(shared: &Shared, snap: &Snapshot) -> Vec<MenuBarItem> {
    let cache = shared.lock().section_cache.clone();
    let items = collect_items(snap, &cache);
    let mut st = shared.lock();
    let reliable = snap.active && !snap.busy && (snap.always_hidden_reliable || snap.always_hidden.is_none());
    if reliable {
        for i in &items {
            st.section_cache.insert(i.id.clone(), i.section);
        }
    }
    st.last_items = items.clone();
    items
}

fn hidden_by_notch(snap: &Snapshot, items: &[MenuBarItem]) -> Vec<MenuBarItem> {
    let (Some(screen), Some(notch)) = (snap.screen, snap.notch) else { return vec![] };
    let shown = if snap.bar_open { Expansion::Collapsed } else { snap.expansion };
    items.iter().filter(|i| shown.shows(i.section) && logic::hidden_by_notch(&i.frame, &screen, &notch)).cloned().collect()
}

pub struct MenuBarManager;

impl MenuBarManager {
    /// Start the manager. Callable from any thread; the status items are
    /// created on the main thread. With `enabled: false` nothing is shown
    /// until [`Handle::update`] enables it.
    pub fn start(config: Config) -> Result<Handle> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                config,
                config_gen: 1,
                requests: vec![],
                stopped: false,
                snapshot: Snapshot { fitted_mode: platform::fitted_mode_default(), ..Snapshot::default() },
                section_cache: HashMap::new(),
                last_items: vec![],
            }),
            changed: Condvar::new(),
        });
        let backend = platform::Backend::start(shared.clone())?;
        Ok(Handle { shared, backend })
    }
}

pub struct Handle {
    shared: Arc<Shared>,
    backend: platform::Backend,
}

impl Handle {
    /// Apply a new configuration.
    pub fn update(&self, config: Config) {
        let mut st = self.shared.lock();
        if st.config != config {
            st.config = config;
            st.config_gen += 1;
            drop(st);
            self.backend.wake();
        }
    }

    /// Restore the menu bar (separators short, our items removed). Idempotent;
    /// also done on drop. Their positions stay remembered for next time.
    pub fn stop(&self) {
        let first = {
            let mut st = self.shared.lock();
            !std::mem::replace(&mut st.stopped, true)
        };
        if first {
            self.backend.stop();
        }
    }

    /// Show the hidden section if it's hidden, hide it otherwise.
    pub fn toggle(&self) {
        self.request(Request::Toggle { all: false });
    }

    pub fn show_hidden(&self) {
        self.request(Request::Show(Expansion::Hidden));
    }

    /// Show the always-hidden section too.
    pub fn show_all(&self) {
        self.request(Request::Show(Expansion::All));
    }

    pub fn hide(&self) {
        self.request(Request::Hide);
    }

    /// Open the floating bar with the hidden (or all hidden) items.
    pub fn show_bar(&self, include_always_hidden: bool) {
        self.request(Request::ShowBar { all: include_always_hidden });
    }

    /// What is shown right now.
    pub fn expansion(&self) -> Expansion {
        self.shared.lock().snapshot.expansion
    }

    /// True on macOS 26+, where hiding uses the longest separator length
    /// that still fits instead of 10 000 pt.
    pub fn fitted_mode(&self) -> bool {
        self.shared.lock().snapshot.fitted_mode
    }

    /// Set when our own items are out of order (e.g. the chevron was
    /// Cmd-dragged left of its separator).
    pub fn layout_issue(&self) -> Option<String> {
        self.shared.lock().snapshot.layout_issue.clone()
    }

    /// All status items, left to right, with their sections. Call it off the
    /// main thread when possible (listing other apps' items on macOS 26+
    /// asks each app over Accessibility).
    pub fn items(&self) -> Vec<MenuBarItem> {
        let snap = self.fresh_snapshot();
        refresh_items(&self.shared, &snap)
    }

    /// Items that should be visible but can't be seen: behind the notch or
    /// pushed left of it. On macOS 26+, called off the main thread with
    /// Accessibility, this also checks with hit tests and includes items the
    /// menu bar moved into its own overflow ("»") because they didn't fit,
    /// on any screen.
    pub fn items_hidden_by_notch(&self) -> Vec<MenuBarItem> {
        let items = self.items();
        let snap = self.shared.lock().snapshot.clone();
        let mut out = hidden_by_notch(&snap, &items);
        if snap.fitted_mode {
            let shown = if snap.bar_open || !snap.active { Expansion::Collapsed } else { snap.expansion };
            for i in &items {
                if !shown.shows(i.section) || out.iter().any(|o| o.id == i.id) || i.frame.w <= 0.0 {
                    continue;
                }
                if platform::really_shown(i.frame, i.pid) == Some(false) {
                    out.push(i.clone());
                }
            }
        }
        out
    }

    /// Move an item by synthesizing a Cmd-drag in the menu bar (needs
    /// Accessibility). Shows every section while moving and restores what
    /// was shown afterwards. Must not be called on the main thread (it waits
    /// for the menu bar to lay out).
    pub fn move_item(&self, item_id: &str, target: MoveTarget) -> Result<()> {
        if platform::is_main_thread() {
            bail!("move_item must be called off the main thread");
        }
        if !platform::permissions().accessibility {
            bail!("moving menu bar items needs the Accessibility permission");
        }
        let before = self.shared.lock().snapshot.expansion;
        self.request(Request::Hold(true));
        let show_all = self.show_and_wait(Expansion::All);
        let res = (|| -> Result<()> {
            show_all?;
            for attempt in 0..3 {
                let items = self.items();
                let snap = self.shared.lock().snapshot.clone();
                let Some(item) = items.iter().find(|i| i.id == item_id) else { bail!("no menu bar item {item_id}") };
                let (target_rect, side) = match &target {
                    MoveTarget::Section(Section::Visible) => (snap.toggle.ok_or_else(|| anyhow::anyhow!("not running"))?, logic::DropSide::RightOf),
                    MoveTarget::Section(Section::Hidden) => (snap.hidden.ok_or_else(|| anyhow::anyhow!("not running"))?, logic::DropSide::LeftOf),
                    MoveTarget::Section(Section::AlwaysHidden) => {
                        (snap.always_hidden.ok_or_else(|| anyhow::anyhow!("the always-hidden section is off"))?, logic::DropSide::LeftOf)
                    }
                    MoveTarget::LeftOf(id) | MoveTarget::RightOf(id) => {
                        let t = items.iter().find(|i| &i.id == id).ok_or_else(|| anyhow::anyhow!("no menu bar item {id}"))?;
                        let side = if matches!(target, MoveTarget::LeftOf(_)) { logic::DropSide::LeftOf } else { logic::DropSide::RightOf };
                        (t.frame, side)
                    }
                };
                if logic::move_succeeded(&item.frame, &target_rect, side) && self.adjacent(&items, item, &target_rect, side) {
                    return Ok(());
                }
                if attempt == 2 {
                    break;
                }
                log::debug!("menubar-manager: moving {} at {:?} {side:?} {target_rect:?} (attempt {attempt})", item.id, item.frame);
                let path = logic::drag_path(&item.frame, &target_rect, side, 12);
                platform::cmd_drag(&path, item.pid)?;
                let gen = self.shared.lock().snapshot.gen;
                self.backend.wake();
                std::thread::sleep(Duration::from_millis(400));
                let _ = self.shared.wait_for(gen, Duration::from_millis(500), |s| !s.busy);
            }
            bail!("the menu bar didn't take the move")
        })();
        match before {
            Expansion::All => {}
            e => {
                let _ = self.show_and_wait(e);
            }
        }
        self.request(Request::Hold(false));
        res
    }

    /// Show the item's section for a moment and click it (as the floating
    /// bar does). Needs Accessibility for other apps' items.
    pub fn press_item(&self, item_id: &str) {
        self.request(Request::Press(item_id.to_string()));
    }

    fn adjacent(&self, items: &[MenuBarItem], item: &MenuBarItem, target: &Rect, side: logic::DropSide) -> bool {
        // Nothing (but our own separators) between the item and the target.
        let (lo, hi) = match side {
            logic::DropSide::LeftOf => (item.frame.right() - 1.0, target.x + 1.0),
            logic::DropSide::RightOf => (target.right() - 1.0, item.frame.x + 1.0),
        };
        !items.iter().any(|o| o.id != item.id && o.frame.w > 0.0 && o.frame.center_x() > lo && o.frame.center_x() < hi && o.frame.y.abs() < 40.0)
    }

    fn show_and_wait(&self, e: Expansion) -> Result<()> {
        let gen = self.shared.lock().snapshot.gen;
        self.request(match e {
            Expansion::Collapsed => Request::Hide,
            e => Request::Show(e),
        });
        if platform::is_main_thread() {
            return Ok(());
        }
        self.shared
            .wait_for(gen, Duration::from_secs(4), |s| !s.active || (s.expansion == e && !s.busy))
            .map(|_| ())
            .ok_or_else(|| anyhow::anyhow!("the menu bar didn't settle"))
    }

    fn fresh_snapshot(&self) -> Snapshot {
        if platform::is_main_thread() {
            platform::refresh_on_main();
            return self.shared.lock().snapshot.clone();
        }
        let gen = self.shared.lock().snapshot.gen;
        self.request(Request::Refresh);
        self.shared.wait_for(gen, Duration::from_millis(500), |_| true).unwrap_or_else(|| self.shared.lock().snapshot.clone())
    }

    fn request(&self, r: Request) {
        self.shared.lock().requests.push(r);
        self.backend.wake();
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_camel_case() {
        let c: Config = serde_json::from_str("{}").unwrap();
        let d = Config::default();
        assert_eq!(c, d);
        assert!(!d.enabled && d.auto_rehide && !d.show_on_hover && !d.use_floating_bar && d.always_hidden_section);
        assert_eq!(d.rehide_delay_secs, 10);
        let v = serde_json::to_value(&d).unwrap();
        for k in ["enabled", "autoRehide", "rehideDelaySecs", "showOnHover", "useFloatingBar", "alwaysHiddenSection", "notchFallbackToBar", "rehideOnFocusChange"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        let p: Config = serde_json::from_str(r#"{"enabled":true,"rehideDelaySecs":3,"unknown":1}"#).unwrap();
        assert!(p.enabled && p.auto_rehide);
        assert_eq!(p.rehide_delay_secs, 3);
        let s = serde_json::to_value(Section::AlwaysHidden).unwrap();
        assert_eq!(s, "alwaysHidden");
    }

    fn snap() -> Snapshot {
        Snapshot {
            active: true,
            toggle: Some(Rect::new(1092.0, 0.0, 30.0, 33.0)),
            hidden: Some(Rect::new(1056.0, 0.0, 36.0, 33.0)),
            always_hidden: Some(Rect::new(900.0, 0.0, 28.0, 33.0)),
            always_hidden_reliable: true,
            screen: Some(Rect::new(0.0, 0.0, 1512.0, 982.0)),
            menu_bar_h: 32.0,
            notch: Some(Rect::new(663.5, 0.0, 185.0, 32.0)),
            ..Default::default()
        }
    }

    fn item(id: &str, x: f64, section: Section) -> MenuBarItem {
        MenuBarItem {
            id: id.into(),
            owner: id.into(),
            bundle_id: None,
            pid: 1,
            title: None,
            frame: Rect::new(x, 4.0, 30.0, 24.0),
            section,
            window_id: None,
            own: false,
            system: false,
        }
    }

    #[test]
    fn notch_report_respects_expansion() {
        let mut s = snap();
        let items = vec![item("v", 1200.0, Section::Visible), item("h", 820.0, Section::Hidden), item("vn", 700.0, Section::Visible)];
        let ids = |v: Vec<MenuBarItem>| v.into_iter().map(|i| i.id).collect::<Vec<_>>();
        // Collapsed: only visible items count.
        assert_eq!(ids(hidden_by_notch(&s, &items)), vec!["vn"]);
        s.expansion = Expansion::Hidden;
        assert_eq!(ids(hidden_by_notch(&s, &items)), vec!["h", "vn"]);
        // Shown through the floating bar: the hidden ones aren't in the bar.
        s.bar_open = true;
        assert_eq!(ids(hidden_by_notch(&s, &items)), vec!["vn"]);
        s.notch = None;
        assert!(hidden_by_notch(&s, &items).is_empty());
    }
}
