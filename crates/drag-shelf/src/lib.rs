//! Drag Shelf: Dropover-style floating shelves.
//!
//! While dragging files (or links, text, images, file promises), shake the
//! pointer left and right and a small shelf springs open under the cursor.
//! Drop the items on it, go wherever they need to go (another window,
//! Space or folder) and drag them out of the shelf there. Several shelves
//! can be open at once; each holds its items until it is closed (shelves
//! with items come back after a restart).
//!
//! * **Detection** ([`shake`], [`session`]): one passive `pm_hook`
//!   subscription ([`pm_hook::priority::DRAG_SHELF`]) watches left-button
//!   drags and never swallows anything. A shake is a few quick horizontal
//!   reversals; it only opens a shelf while the drag pasteboard holds
//!   something new that a shelf takes (moving a window or selecting text
//!   does nothing).
//! * **Shelves** (macOS): borderless non-activating panels on every Space
//!   and over full-screen apps, styled like the Notch Shelf. The items show
//!   as a fanned stack of Quick Look thumbnails with a count; drag the stack
//!   to take everything, or expand it to a grid to pick one. Copy to… /
//!   Move to… a folder, Open, Show in Finder, Copy, Share (AirDrop), Remove,
//!   Clear; empty shelves close by themselves.
//! * **Other triggers**: [`Handle::new_shelf_at_cursor`] (the tray's "New
//!   drag shelf") and an optional global hotkey ([`Config::hotkey`]).
//!
//! The item model, drop parsing, thumbnails, drag-out writers and sharing
//! are shared with the Notch Shelf through `pm_shelf_core`.
//!
//! **Windows** isn't supported yet ([`SUPPORTED`] is false): the config
//! loads and the handle works as a model without any UI.

pub mod fileops;
pub mod layout;
pub mod model;
mod platform;
pub mod session;
pub mod shake;

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use model::{summary, ShelfSet};
pub use pm_shelf_core::geom::{Point, Rect};
pub use pm_shelf_core::store::{ItemKind, NewItem, ShelfItem};
pub use shake::{Sensitivity, ShakeDetector, ShakeThresholds};

/// Whether this platform has drag shelves (macOS).
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// Why the module does nothing here (`None` where it is supported).
pub fn unsupported_reason() -> Option<&'static str> {
    (!SUPPORTED).then_some(if cfg!(windows) { "not supported on Windows" } else { "not supported on this platform" })
}

/// A global hotkey in PowerToys' JSON shape (`win` is Command on macOS,
/// `alt` is Option; `code` is a Windows virtual-key code; 0 = none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Hotkey {
    pub win: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub code: u32,
}

impl Hotkey {
    pub const fn new(win: bool, ctrl: bool, alt: bool, shift: bool, code: u32) -> Self {
        Self { win, ctrl, alt, shift, code }
    }

    pub fn is_set(&self) -> bool {
        self.code != 0
    }

    /// Whether a key press (VK and modifiers) is this hotkey.
    pub fn matches(&self, vk: u32, win: bool, ctrl: bool, alt: bool, shift: bool) -> bool {
        self.is_set() && self.code == vk && self.win == win && self.ctrl == ctrl && self.alt == alt && self.shift == shift
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mac = cfg!(target_os = "macos");
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push(if mac { "Option" } else { "Alt" }.into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        if self.win {
            parts.push(if mac { "Cmd" } else { "Win" }.into());
        }
        parts.push(match self.code {
            0 => "(none)".to_string(),
            c @ (0x30..=0x39 | 0x41..=0x5A) => char::from_u32(c).unwrap().to_string(),
            c @ 0x70..=0x87 => format!("F{}", c - 0x6F),
            0x20 => "Space".into(),
            0xC0 => "`".into(),
            c => format!("VK 0x{c:02X}"),
        });
        write!(f, "{}", parts.join("+"))
    }
}

/// `dragShelf` section of the app config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Default: on where supported (macOS).
    pub enabled: bool,
    /// Open a shelf when the pointer is shaken during a drag.
    pub shake_to_open: bool,
    pub sensitivity: Sensitivity,
    /// The thresholds used with `sensitivity: custom`.
    pub custom_shake: ShakeThresholds,
    /// Take items off the shelf once they were dropped somewhere.
    pub remove_after_drag_out: bool,
    /// Close a shelf by itself once it is emptied.
    pub auto_close: bool,
    /// How long an emptied shelf stays before it closes.
    pub auto_close_delay_ms: u64,
    /// Opens an empty shelf at the pointer. None by default.
    pub hotkey: Hotkey,
    /// Shelves open at most at once (1–20).
    pub max_shelves: u32,
    /// Copy files dropped from temporary places (screenshot previews,
    /// downloads in progress, …) so they don't disappear.
    pub copy_temporary_files: bool,
    /// Trackpad tick when a shelf opens and when items land on it.
    pub haptics: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: SUPPORTED,
            shake_to_open: true,
            sensitivity: Sensitivity::Medium,
            custom_shake: ShakeThresholds::default(),
            remove_after_drag_out: true,
            auto_close: true,
            auto_close_delay_ms: 800,
            hotkey: Hotkey::default(),
            max_shelves: 5,
            copy_temporary_files: true,
            haptics: true,
        }
    }
}

impl Config {
    /// The shake thresholds in effect.
    pub fn thresholds(&self) -> ShakeThresholds {
        self.sensitivity.thresholds(&self.custom_shake)
    }

    pub fn max_shelves(&self) -> usize {
        (self.max_shelves as usize).clamp(1, model::MAX_SHELVES)
    }

    pub fn auto_close_delay(&self) -> Duration {
        Duration::from_millis(self.auto_close_delay_ms.min(60_000))
    }
}

/// A shelf as [`Handle::shelves`] reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct ShelfInfo {
    pub id: u64,
    pub items: Vec<ShelfItem>,
    /// Top-left of the panel (global, top-left origin, points).
    pub origin: Point,
    pub expanded: bool,
}

/// Requests from the [`Handle`] to the UI thread.
#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum Request {
    /// Render every shelf offscreen to `<dir>/drag-shelf-<n>.png`.
    RenderPng(PathBuf),
    Debug(String),
}

pub(crate) struct State {
    pub config: Config,
    pub config_gen: u64,
    pub set: ShelfSet,
    /// Bumped on every change of the shelves or their items.
    pub gen: u64,
    /// New shelves that still need a place on screen: (id, cursor).
    pub to_place: Vec<(u64, Option<Point>)>,
    pub requests: Vec<Request>,
    pub stopped: bool,
}

pub(crate) struct Shared {
    state: Mutex<State>,
    /// Shake detection on (read by the hook thread).
    pub shake_on: AtomicBool,
    pub thresholds: Mutex<ShakeThresholds>,
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Change the shelves; bumps the generation.
    pub fn edit<R>(&self, f: impl FnOnce(&mut ShelfSet) -> R) -> R {
        let mut st = self.lock();
        let r = f(&mut st.set);
        st.gen += 1;
        r
    }

    /// Open a new shelf (for `cursor`, placed by the UI) with `items`.
    pub fn new_shelf(&self, cursor: Option<Point>, items: Vec<NewItem>) -> Option<u64> {
        let mut st = self.lock();
        let max = st.config.max_shelves();
        let origin = cursor.unwrap_or_default();
        let id = st.set.create(origin, max)?;
        if !items.is_empty() {
            let opts = st.set.add_options(st.config.copy_temporary_files);
            st.set.add(id, items, &opts);
        }
        st.to_place.push((id, cursor));
        st.gen += 1;
        Some(id)
    }

    pub fn add(&self, shelf: u64, items: Vec<NewItem>) -> Vec<u64> {
        let mut st = self.lock();
        let opts = st.set.add_options(st.config.copy_temporary_files);
        let ids = st.set.add(shelf, items, &opts);
        st.gen += 1;
        ids
    }
}

pub struct DragShelf;

impl DragShelf {
    /// Start the module. `data_dir` gets the `drag-shelves` folder.
    /// Callable from any thread; the panels are made on the main thread
    /// (the host must run the main run loop, as the PowerMac tray app does).
    pub fn start(config: Config, data_dir: PathBuf) -> Result<Handle> {
        std::fs::create_dir_all(&data_dir)?;
        let set = ShelfSet::open(&data_dir);
        let restored: Vec<(u64, Option<Point>)> = set.ids().into_iter().map(|id| (id, None)).collect();
        let shared = Arc::new(Shared {
            state: Mutex::new(State { config: config.clone(), config_gen: 1, set, gen: 1, to_place: restored, requests: vec![], stopped: false }),
            shake_on: AtomicBool::new(false),
            thresholds: Mutex::new(config.thresholds()),
        });
        let ui = config.enabled && SUPPORTED;
        let backend = if ui { Some(platform::Backend::start(shared.clone())?) } else { None };
        if let Some(why) = unsupported_reason() {
            if config.enabled {
                log::info!("drag shelf: {why}");
            }
        }
        let h = Handle { shared, backend: Mutex::new(backend), hooks: Mutex::new(Hooks::default()) };
        h.apply_hooks(&config);
        Ok(h)
    }
}

#[derive(Default)]
struct Hooks {
    drag: Option<pm_hook::Guard>,
    hotkey: Option<(Hotkey, pm_hook::Guard)>,
}

pub struct Handle {
    shared: Arc<Shared>,
    backend: Mutex<Option<platform::Backend>>,
    hooks: Mutex<Hooks>,
}

impl Handle {
    /// Apply a new configuration.
    pub fn update(&self, config: Config) {
        {
            let mut st = self.shared.lock();
            if st.config == config {
                return;
            }
            st.config = config.clone();
            st.config_gen += 1;
        }
        let ui = config.enabled && SUPPORTED;
        {
            let mut b = self.backend.lock().unwrap_or_else(|e| e.into_inner());
            match (ui, b.is_some()) {
                (true, false) => match platform::Backend::start(self.shared.clone()) {
                    Ok(be) => *b = Some(be),
                    Err(e) => log::error!("drag shelf: {e:#}"),
                },
                (false, true) => {
                    if let Some(be) = b.take() {
                        be.stop();
                    }
                }
                _ => {}
            }
        }
        self.apply_hooks(&config);
        self.wake();
    }

    fn apply_hooks(&self, config: &Config) {
        let ui = config.enabled && SUPPORTED;
        *self.shared.thresholds.lock().unwrap_or_else(|e| e.into_inner()) = config.thresholds();
        self.shared.shake_on.store(ui && config.shake_to_open, Ordering::Relaxed);
        let mut hooks = self.hooks.lock().unwrap_or_else(|e| e.into_inner());
        if ui && config.shake_to_open {
            if hooks.drag.is_none() {
                hooks.drag = Some(platform::drag_watch(self.shared.clone()));
            }
        } else {
            hooks.drag = None;
        }
        let want = (ui && config.hotkey.is_set()).then_some(config.hotkey);
        if hooks.hotkey.as_ref().map(|(h, _)| *h) != want {
            hooks.hotkey = None; // unsubscribes
            if let Some(hk) = want {
                log::info!("drag shelf: new shelf with {hk}");
                hooks.hotkey = Some((hk, platform::hotkey_watch(hk)));
            }
        }
    }

    fn wake(&self) {
        if let Some(b) = self.backend.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            b.wake();
        }
    }

    /// Close every shelf window and stop listening (the shelves are kept
    /// and come back at the next start). Idempotent; also done on drop.
    pub fn stop(&self) {
        let first = {
            let mut st = self.shared.lock();
            !std::mem::replace(&mut st.stopped, true)
        };
        if first {
            *self.hooks.lock().unwrap_or_else(|e| e.into_inner()) = Hooks::default();
            self.shared.shake_on.store(false, Ordering::Relaxed);
            if let Some(b) = self.backend.lock().unwrap_or_else(|e| e.into_inner()).take() {
                b.stop();
            }
        }
    }

    /// Open an empty shelf at the pointer (tray item, hotkey). `None` when
    /// the maximum number of shelves is open.
    pub fn new_shelf_at_cursor(&self) -> Option<u64> {
        self.new_shelf(platform::cursor(), vec![])
    }

    /// Open a shelf for the pointer at `at` (global, top-left origin) with
    /// `items` on it.
    pub fn new_shelf(&self, at: Option<Point>, items: Vec<NewItem>) -> Option<u64> {
        let id = self.shared.new_shelf(at, items);
        match id {
            Some(id) => log::info!("drag shelf: opened shelf {id}"),
            None => log::info!("drag shelf: {} shelves are open already", self.shared.lock().config.max_shelves()),
        }
        self.wake();
        id
    }

    /// Put items on shelf `shelf`; returns the ids of the items added.
    pub fn add(&self, shelf: u64, items: Vec<NewItem>) -> Vec<u64> {
        let ids = self.shared.add(shelf, items);
        self.wake();
        ids
    }

    pub fn remove(&self, shelf: u64, items: &[u64]) {
        self.shared.edit(|s| s.remove_items(shelf, items));
        self.wake();
    }

    /// Close shelf `id` (its items are dropped).
    pub fn close_shelf(&self, id: u64) {
        self.shared.edit(|s| s.close(id));
        self.wake();
    }

    /// The open shelves, oldest first.
    pub fn shelves(&self) -> Vec<ShelfInfo> {
        let st = self.shared.lock();
        st.set.shelves().iter().map(|s| ShelfInfo { id: s.id, items: s.items().to_vec(), origin: s.origin, expanded: s.expanded }).collect()
    }

    /// Whether shaking during a drag currently opens shelves.
    pub fn listening(&self) -> bool {
        self.hooks.lock().unwrap_or_else(|e| e.into_inner()).drag.is_some()
    }

    /// One line for the app's status ("2 shelves open", "not supported on
    /// Windows").
    pub fn status_line(&self) -> String {
        if let Some(why) = unsupported_reason() {
            return why.into();
        }
        let n = self.shared.lock().set.len();
        let shake = if self.listening() { "shake while dragging" } else { "shake off" };
        match n {
            0 => shake.into(),
            1 => format!("1 shelf open · {shake}"),
            n => format!("{n} shelves open · {shake}"),
        }
    }

    /// Diagnostics: render each shelf offscreen to
    /// `<dir>/drag-shelf-<n>.png` on the next frame (macOS only).
    #[doc(hidden)]
    pub fn render_png(&self, dir: PathBuf) {
        self.request(Request::RenderPng(dir));
    }

    /// Diagnostics: "dropHover:N" / "noDropHover", "grid:N" / "stack:N",
    /// "hover:N:K" (hover tile K of shelf N), "select:N:K", "flash:N:TEXT",
    /// "shake:X:Y" (as if a shake was detected at X, Y), "fresh" (no
    /// appear animation for the next shelves).
    #[doc(hidden)]
    pub fn debug(&self, cmd: &str) {
        self.request(Request::Debug(cmd.to_string()));
    }

    fn request(&self, r: Request) {
        self.shared.lock().requests.push(r);
        self.wake();
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
        assert_eq!(c, Config::default());
        assert_eq!(c.enabled, SUPPORTED);
        assert!(c.shake_to_open && c.remove_after_drag_out && c.auto_close && !c.hotkey.is_set());
        assert_eq!((c.sensitivity, c.max_shelves, c.auto_close_delay_ms), (Sensitivity::Medium, 5, 800));
        let v = serde_json::to_value(&c).unwrap();
        for k in ["enabled", "shakeToOpen", "sensitivity", "customShake", "removeAfterDragOut", "autoClose", "autoCloseDelayMs", "hotkey", "maxShelves", "copyTemporaryFiles", "haptics"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert_eq!(v["sensitivity"], "medium");
        let p: Config = serde_json::from_str(r#"{"sensitivity":"custom","customShake":{"reversals":6},"maxShelves":99,"hotkey":{"ctrl":true,"alt":true,"code":68},"unknown":1}"#).unwrap();
        assert_eq!(p.thresholds().reversals, 6);
        assert_eq!(p.max_shelves(), model::MAX_SHELVES);
        assert_eq!(p.hotkey, Hotkey::new(false, true, true, false, 68));
        assert!(p.hotkey.matches(68, false, true, true, false) && !p.hotkey.matches(68, false, true, true, true));
        assert_eq!(Config { max_shelves: 0, ..Config::default() }.max_shelves(), 1);
        assert_eq!(Config { auto_close_delay_ms: u64::MAX, ..Config::default() }.auto_close_delay(), Duration::from_secs(60));
        let mac = cfg!(target_os = "macos");
        assert_eq!(p.hotkey.to_string(), if mac { "Ctrl+Option+D" } else { "Ctrl+Alt+D" });
        assert_eq!(Hotkey::default().to_string(), "(none)");
    }

    #[test]
    fn handle_api_without_ui() {
        let dir = std::env::temp_dir().join(format!("drag-shelf-handle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Disabled: no panels and no hooks; the model still works.
        let cfg = Config { enabled: false, max_shelves: 2, ..Config::default() };
        let h = DragShelf::start(cfg.clone(), dir.clone()).unwrap();
        assert!(!h.listening());
        let a = h.new_shelf(Some(Point::new(400.0, 300.0)), vec![NewItem::Text("hello".into())]).unwrap();
        let b = h.new_shelf(None, vec![]).unwrap();
        assert_eq!(h.new_shelf(None, vec![]), None, "max 2");
        let ids = h.add(b, vec![NewItem::Url { url: "https://a.b".into(), title: None }, NewItem::Text("x".into())]);
        assert_eq!(ids.len(), 2);
        let s = h.shelves();
        assert_eq!(s.iter().map(|s| (s.id, s.items.len())).collect::<Vec<_>>(), vec![(a, 1), (b, 2)]);
        h.remove(b, &ids[..1]);
        assert_eq!(h.shelves()[1].items.len(), 1);
        h.close_shelf(a);
        assert_eq!(h.shelves().len(), 1);
        h.update(Config { enabled: false, max_shelves: 3, ..Config::default() });
        h.debug("grid:0");
        if SUPPORTED {
            assert_eq!(h.status_line(), "1 shelf open · shake off");
        } else {
            assert!(h.status_line().starts_with("not supported"));
        }
        h.stop();
        drop(h);
        // b comes back with its item.
        let h2 = DragShelf::start(cfg, dir.clone()).unwrap();
        let s = h2.shelves();
        assert_eq!((s.len(), s[0].id, s[0].items.len()), (1, b, 1));
        drop(h2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
