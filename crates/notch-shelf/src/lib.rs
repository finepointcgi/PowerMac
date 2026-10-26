//! Notch Shelf: turns the MacBook camera housing ("notch") into a drop
//! shelf, in the spirit of NotchNook / Dropover / Yoink.
//!
//! * **macOS**: a borderless, non-activating `NSPanel` above the menu bar on
//!   every Space (and over full-screen apps). Collapsed it is a black shape
//!   exactly over the notch, so it can't be seen. It springs open into a
//!   rounded black panel when the pointer rests on the notch, when the notch
//!   is clicked, or when a drag carrying files / URLs / text / images comes
//!   near the top of the screen. Dropped items become tiles (Quick Look
//!   thumbnails) that can be dragged back out to any app, opened,
//!   revealed in Finder, shared (AirDrop etc.) or removed. The shelf is
//!   persisted as JSON in the data dir. Screens without a notch can get a
//!   "virtual notch" pill at the top center.
//! * **Windows**: a top-center drop shelf (layered window) that slides down
//!   when files are dragged to the top center of the primary monitor; OLE
//!   drop target and drag source.
//! * Elsewhere: a no-op backend.
//!
//! All AppKit work happens on the main thread through the main dispatch
//! queue; the host must run `NSApplication` (or at least the main run loop)
//! on the main thread, as the PowerMac tray app does.

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod anim;
pub mod geometry;
pub mod logic;
mod platform;
pub mod store;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use store::{ItemKind, NewItem, ShelfItem};

/// Which screens get a shelf.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ScreenMode {
    /// Screens with a notch. If there is none (lid closed, external
    /// displays only) and `virtual_notch_on_non_notch_screens` is on, the
    /// main screen gets a virtual notch.
    #[default]
    NotchOnly,
    /// Every screen: the notch where there is one, a virtual notch on the
    /// others when `virtual_notch_on_non_notch_screens` is on.
    All,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Default: true on Macs whose built-in display has a notch.
    pub enabled: bool,
    /// Expand when the pointer rests on the notch.
    pub expand_on_hover: bool,
    pub hover_delay_ms: u64,
    /// How long the pointer must be away before the panel closes.
    pub collapse_delay_ms: u64,
    /// Expand when a drag with files / URLs / text / images nears the top.
    pub expand_on_drag: bool,
    /// Expand when the notch is clicked.
    pub expand_on_click: bool,
    /// Give screens without a notch a top-center pill.
    pub virtual_notch_on_non_notch_screens: bool,
    pub screens: ScreenMode,
    /// Copy files dropped from temporary locations (screenshot previews,
    /// `/var/folders`, caches) into the shelf's own folder so they survive.
    pub copy_temporary_files: bool,
    /// Take items off the shelf once they were dropped somewhere else.
    pub remove_after_drag_out: bool,
    /// Trackpad haptic tick on drop.
    pub haptics: bool,
    /// Width of the expanded panel in points.
    pub panel_width: u32,
    /// Extra: show the current clipboard item as the first tile (reads the
    /// general pasteboard when the panel opens; on recent macOS this can
    /// show a paste-permission prompt). Off by default.
    pub show_clipboard: bool,
    /// Extra: briefly widen the notch with the battery level when the
    /// charger is connected or disconnected. Off by default.
    pub battery_indicator: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: has_notch(),
            expand_on_hover: true,
            hover_delay_ms: 150,
            collapse_delay_ms: 350,
            expand_on_drag: true,
            expand_on_click: true,
            virtual_notch_on_non_notch_screens: false,
            screens: ScreenMode::NotchOnly,
            copy_temporary_files: true,
            remove_after_drag_out: false,
            haptics: true,
            panel_width: 620,
            show_clipboard: false,
            battery_indicator: false,
        }
    }
}

impl Config {
    pub(crate) fn logic_settings(&self) -> logic::Settings {
        logic::Settings {
            expand_on_hover: self.expand_on_hover,
            hover_delay: self.hover_delay_ms.min(10_000) as f64 / 1000.0,
            collapse_delay: self.collapse_delay_ms.min(10_000) as f64 / 1000.0,
            expand_on_drag: self.expand_on_drag,
            expand_on_click: self.expand_on_click,
        }
    }

    pub(crate) fn metrics(&self) -> geometry::Metrics {
        geometry::Metrics { panel_width: (self.panel_width as f64).clamp(380.0, 1400.0), ..Default::default() }
    }

    pub(crate) fn add_options(&self) -> store::AddOptions {
        store::AddOptions { copy_temporary_files: self.copy_temporary_files, temp_roots: store::default_temp_roots() }
    }
}

/// Whether this machine's built-in display has a notch (any thread).
pub fn has_notch() -> bool {
    platform::has_notch()
}

/// Requests from the [`Handle`] to the UI thread.
#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum Request {
    Expand,
    Collapse,
    /// Render every panel offscreen to `<dir>/notch-shelf-<n>.png`.
    RenderPng(PathBuf),
    /// Diagnostics: "dropHover", "noDropHover", "peek".
    Debug(String),
}

pub(crate) struct State {
    pub config: Config,
    pub config_gen: u64,
    pub store: store::Store,
    pub items_gen: u64,
    pub requests: Vec<Request>,
    pub stopped: bool,
}

pub(crate) struct Shared {
    state: Mutex<State>,
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Mutate the store; bumps the generation and saves when `f` says so.
    pub fn edit(&self, f: impl FnOnce(&mut store::Store) -> bool) -> bool {
        let mut st = self.lock();
        let changed = f(&mut st.store);
        if changed {
            st.items_gen += 1;
            if let Err(e) = st.store.save() {
                log::warn!("notch-shelf: saving shelf: {e:#}");
            }
        }
        changed
    }

    /// Add items with the current config's options; returns the new ids.
    pub fn add(&self, items: Vec<NewItem>) -> Vec<u64> {
        let opts = self.lock().config.add_options();
        let mut ids = vec![];
        self.edit(|s| {
            for it in items {
                match s.add(it, &opts) {
                    Ok(id) => ids.push(id),
                    Err(e) => log::info!("notch-shelf: not added: {e:#}"),
                }
            }
            !ids.is_empty()
        });
        ids
    }
}

pub struct NotchShelf;

impl NotchShelf {
    /// Start the shelf. `data_dir` holds `notch-shelf.json` and the
    /// `notch-shelf-files` folder. Callable from any thread; the UI is
    /// created on the main thread.
    pub fn start(config: Config, data_dir: PathBuf) -> Result<Handle> {
        std::fs::create_dir_all(&data_dir)?;
        let store = store::Store::open(&data_dir);
        let shared = Arc::new(Shared {
            state: Mutex::new(State { config, config_gen: 1, store, items_gen: 1, requests: vec![], stopped: false }),
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
    /// Apply a new configuration (takes effect on the next frame).
    pub fn update(&self, config: Config) {
        let mut st = self.shared.lock();
        if st.config != config {
            st.config = config;
            st.config_gen += 1;
            drop(st);
            self.backend.wake();
        }
    }

    /// Close the shelf windows. Idempotent; also done on drop.
    pub fn stop(&self) {
        let first = {
            let mut st = self.shared.lock();
            !std::mem::replace(&mut st.stopped, true)
        };
        if first {
            self.backend.stop();
        }
    }

    /// Current shelf contents, in display order.
    pub fn items(&self) -> Vec<ShelfItem> {
        self.shared.lock().store.items().to_vec()
    }

    /// Remove everything from the shelf.
    pub fn clear(&self) {
        if self.shared.edit(|s| s.clear()) {
            self.backend.wake();
        }
    }

    /// Put items on the shelf (e.g. from a service or a hotkey); returns
    /// the ids of the items added.
    pub fn add(&self, items: Vec<NewItem>) -> Vec<u64> {
        let ids = self.shared.add(items);
        self.backend.wake();
        ids
    }

    pub fn remove(&self, ids: &[u64]) {
        if self.shared.edit(|s| s.remove(ids)) {
            self.backend.wake();
        }
    }

    /// Open the panel on every shelf screen (e.g. from a tray item).
    pub fn expand(&self) {
        self.request(Request::Expand);
    }

    pub fn collapse(&self) {
        self.request(Request::Collapse);
    }

    /// Diagnostics: render each panel offscreen (over a mock menu bar) to
    /// `<dir>/notch-shelf-<n>.png` on the next frame (macOS only).
    pub fn render_png(&self, dir: PathBuf) {
        self.request(Request::RenderPng(dir));
    }

    /// Diagnostics for previews: "dropHover" / "noDropHover" (show the
    /// drop-target state), "peek" (battery peek).
    #[doc(hidden)]
    pub fn debug(&self, cmd: &str) {
        self.request(Request::Debug(cmd.to_string()));
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
        assert!(d.expand_on_hover && d.expand_on_drag && d.copy_temporary_files);
        assert!(!d.virtual_notch_on_non_notch_screens && !d.remove_after_drag_out && !d.show_clipboard && !d.battery_indicator);
        assert_eq!(d.screens, ScreenMode::NotchOnly);
        let v = serde_json::to_value(&d).unwrap();
        for k in ["enabled", "expandOnHover", "hoverDelayMs", "collapseDelayMs", "expandOnDrag", "virtualNotchOnNonNotchScreens", "copyTemporaryFiles", "removeAfterDragOut", "screens"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert_eq!(v["screens"], "notchOnly");
        let all: Config = serde_json::from_str(r#"{"screens":"all","hoverDelayMs":5,"unknown":1}"#).unwrap();
        assert_eq!(all.screens, ScreenMode::All);
        assert_eq!(all.hover_delay_ms, 5);
        assert!((all.logic_settings().hover_delay - 0.005).abs() < 1e-12);
        let wide = Config { panel_width: 5000, ..d.clone() };
        assert_eq!(wide.metrics().panel_width, 1400.0);
    }

    #[test]
    fn handle_api_without_ui() {
        let dir = std::env::temp_dir().join(format!("notch-shelf-handle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Disabled config: no UI is created; the store API still works.
        let h = NotchShelf::start(Config { enabled: false, ..Default::default() }, dir.clone()).unwrap();
        let ids = h.add(vec![NewItem::Text("hello".into()), NewItem::Url { url: "https://a.b".into(), title: None }]);
        assert_eq!(ids.len(), 2);
        assert_eq!(h.items().len(), 2);
        h.remove(&ids[..1]);
        assert_eq!(h.items().len(), 1);
        h.update(Config { enabled: false, hover_delay_ms: 10, ..Default::default() });
        h.stop();
        drop(h);
        let h2 = NotchShelf::start(Config { enabled: false, ..Default::default() }, dir.clone()).unwrap();
        assert_eq!(h2.items().len(), 1);
        h2.clear();
        assert!(h2.items().is_empty());
        drop(h2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
