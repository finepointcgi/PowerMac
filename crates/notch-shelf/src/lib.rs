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
pub mod widgets;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use store::{ItemKind, NewItem, ShelfItem};
pub use widgets::{
    Access, CalendarConfig, DefaultView, NowPlayingConfig, QuickAction, QuickToggle, StartOptions, TempUnit, View, WeatherConfig, WidgetConfig,
    WidgetKind, WidgetStatus,
};

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
    /// Widgets of the Home view; the order is the display order. With no
    /// widget enabled the notch only has the shelf (no view switcher).
    pub widgets: Vec<WidgetConfig>,
    /// The view the notch opens in (a drag always opens the shelf).
    pub default_view: DefaultView,
    pub weather: WeatherConfig,
    pub calendar: CalendarConfig,
    pub now_playing: NowPlayingConfig,
    /// Show music / timer "live activities" beside the collapsed notch.
    pub live_activities: bool,
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
            widgets: WidgetConfig::defaults(),
            default_view: DefaultView::Last,
            weather: WeatherConfig::default(),
            calendar: CalendarConfig::default(),
            now_playing: NowPlayingConfig::default(),
            live_activities: true,
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

    /// Enabled widget kinds in display order.
    pub fn enabled_widgets(&self) -> Vec<WidgetKind> {
        WidgetConfig::enabled_kinds(&self.widgets)
    }

    pub fn widget_enabled(&self, k: WidgetKind) -> bool {
        self.enabled_widgets().contains(&k)
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
    SetView(View),
    RefreshWidgets,
}

pub(crate) struct State {
    pub config: Config,
    pub config_gen: u64,
    pub store: store::Store,
    pub items_gen: u64,
    pub requests: Vec<Request>,
    pub stopped: bool,
}

/// Wakes the provider threads early (refresh requests, commands).
#[derive(Default)]
pub(crate) struct Signal {
    gen: Mutex<u64>,
    cv: Condvar,
}

impl Signal {
    pub fn notify(&self) {
        *self.gen.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.cv.notify_all();
    }

    /// Sleep up to `d` or until notified.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn wait(&self, d: Duration) {
        let g = self.gen.lock().unwrap_or_else(|e| e.into_inner());
        let start = *g;
        let _ = self.cv.wait_timeout_while(g, d, |g| *g == start);
    }
}

/// Requests from the UI to the provider threads.
#[derive(Default)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct ProviderReqs {
    pub media: Vec<widgets::media::Command>,
    pub refresh_calendar: bool,
    pub refresh_weather: bool,
    pub calendar_ask: bool,
}

pub(crate) struct Shared {
    state: Mutex<State>,
    /// Host-provided toggles / actions.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub host: StartOptions,
    /// Latest provider data; `data_gen` bumps on every change.
    pub data: Mutex<widgets::WidgetData>,
    pub data_gen: AtomicU64,
    pub reqs: Mutex<ProviderReqs>,
    pub signal: Signal,
    /// The Home view is on screen (providers poll faster).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub home_visible: AtomicBool,
    /// Demo data instead of the real providers (renders / previews).
    pub demo: AtomicBool,
}

impl Shared {
    pub fn data(&self) -> MutexGuard<'_, widgets::WidgetData> {
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Change provider data (ignored while showing demo data unless `demo`).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn set_data(&self, demo: bool, f: impl FnOnce(&mut widgets::WidgetData)) {
        if self.demo.load(Ordering::Relaxed) != demo {
            return;
        }
        f(&mut self.data());
        self.data_gen.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn reqs(&self) -> MutexGuard<'_, ProviderReqs> {
        self.reqs.lock().unwrap_or_else(|e| e.into_inner())
    }

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
        Self::start_with(config, data_dir, StartOptions::default())
    }

    /// Like [`NotchShelf::start`], with quick toggles / actions for the
    /// Home view's Quick Controls widget. `data_dir` also gets
    /// `notch-widgets.json` (last view, weather cache) and `notch-note.txt`.
    pub fn start_with(config: Config, data_dir: PathBuf, options: StartOptions) -> Result<Handle> {
        std::fs::create_dir_all(&data_dir)?;
        let store = store::Store::open(&data_dir);
        let shared = Arc::new(Shared {
            state: Mutex::new(State { config, config_gen: 1, store, items_gen: 1, requests: vec![], stopped: false }),
            host: options,
            data: Mutex::new(widgets::WidgetData::default()),
            data_gen: AtomicU64::new(1),
            reqs: Mutex::new(ProviderReqs::default()),
            signal: Signal::default(),
            home_visible: AtomicBool::new(false),
            demo: AtomicBool::new(false),
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
            self.shared.signal.notify();
        }
    }

    /// Switch the expanded notch to `view` (opens nothing by itself; the
    /// choice is remembered as the last view).
    pub fn set_view(&self, view: View) {
        self.request(Request::SetView(view));
    }

    /// Re-read the quick toggles and refetch calendar events and weather
    /// (weather only if the cache is older than a few minutes).
    pub fn refresh_widgets(&self) {
        {
            let mut r = self.shared.reqs.lock().unwrap_or_else(|e| e.into_inner());
            r.refresh_calendar = true;
            r.refresh_weather = true;
        }
        self.shared.signal.notify();
        self.request(Request::RefreshWidgets);
    }

    /// Permission / source states of the widgets (for a settings page).
    pub fn widget_status(&self) -> WidgetStatus {
        let d = self.shared.data();
        WidgetStatus { calendar: d.calendar_access, camera: d.camera_access, media_source: d.now_playing.as_ref().map(|n| n.source) }
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
    /// drop-target state), "peek" (battery peek), "demo" (fake widget data
    /// instead of the real providers; no permissions needed), "timer:SECS",
    /// "stopwatch", "timerDone", "timerReset", "hover:N" (hover the Nth
    /// quick control), "note:TEXT".
    #[doc(hidden)]
    pub fn debug(&self, cmd: &str) {
        self.request(Request::Debug(cmd.to_string()));
    }

    /// Diagnostics: run `cmd` (see [`Handle::debug`]) and return once the
    /// UI thread has processed it (best effort; waits up to 1 s).
    #[doc(hidden)]
    pub fn debug_sync(&self, cmd: &str) {
        self.debug(cmd);
        for _ in 0..100 {
            if self.shared.lock().requests.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
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
    fn old_config_json_still_loads_with_widget_defaults() {
        // A config written before widgets existed.
        let old = r#"{"enabled":true,"expandOnHover":false,"hoverDelayMs":300,"collapseDelayMs":350,"expandOnDrag":true,
            "expandOnClick":true,"virtualNotchOnNonNotchScreens":true,"screens":"all","copyTemporaryFiles":true,
            "removeAfterDragOut":false,"haptics":true,"panelWidth":700,"showClipboard":false,"batteryIndicator":true}"#;
        let c: Config = serde_json::from_str(old).unwrap();
        assert!(c.enabled && !c.expand_on_hover && c.battery_indicator);
        assert_eq!((c.hover_delay_ms, c.panel_width, c.screens), (300, 700, ScreenMode::All));
        assert_eq!(c.widgets, WidgetConfig::defaults());
        assert_eq!(c.default_view, DefaultView::Last);
        assert!(c.live_activities && !c.now_playing.use_applescript);
        assert_eq!(c.calendar.days, 2);
        assert!(c.weather.location.is_empty());
        assert_eq!(c.enabled_widgets(), vec![WidgetKind::NowPlaying, WidgetKind::Battery, WidgetKind::Timer, WidgetKind::QuickToggles]);
        // Round trip with the new fields.
        let v = serde_json::to_value(&c).unwrap();
        for k in ["widgets", "defaultView", "weather", "calendar", "nowPlaying", "liveActivities"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert_eq!(v["widgets"][0], serde_json::json!({"kind": "nowPlaying", "enabled": true}));
        assert_eq!(v["nowPlaying"]["useApplescript"], false);
        let back: Config = serde_json::from_value(v).unwrap();
        assert_eq!(back, c);
        // New fields, partially given; unknown widget kinds don't break loading.
        let newer = r#"{"widgets":[{"kind":"weather","enabled":true},{"kind":"teleporter","enabled":true},{"kind":"timer","enabled":false}],
            "defaultView":"shelf","weather":{"location":"Berlin","units":"fahrenheit"},"calendar":{"calendars":["Work"]},
            "nowPlaying":{"useApplescript":true},"liveActivities":false}"#;
        let n: Config = serde_json::from_str(newer).unwrap();
        assert_eq!(n.enabled_widgets(), vec![WidgetKind::Weather]);
        assert!(n.widget_enabled(WidgetKind::Weather) && !n.widget_enabled(WidgetKind::Timer));
        assert_eq!(n.default_view, DefaultView::Shelf);
        assert_eq!(n.weather.units, TempUnit::Fahrenheit);
        assert_eq!((n.calendar.days, n.calendar.calendars.clone()), (2, vec!["Work".to_string()]));
        assert!(n.now_playing.use_applescript && !n.live_activities);
        // An empty list means no widgets (shelf only).
        let none: Config = serde_json::from_str(r#"{"widgets":[]}"#).unwrap();
        assert!(none.enabled_widgets().is_empty());
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
        h.set_view(View::Shelf);
        h.refresh_widgets();
        assert_eq!(h.widget_status().media_source, None);
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
