//! Widgets for the notch's "Home" view (NotchNook / Alcove style): now
//! playing, calendar, weather, battery, system stats, timer & stopwatch,
//! quick toggles from the host app, a scratchpad note and a camera mirror.
//!
//! Everything in this module is platform independent: configuration, the
//! data the providers deliver, the pure logic (timer state machine, rate
//! computation, weather codes, layout) and the drawing model ([`draw`]),
//! which turns widget state into a list of primitives the platform layer
//! paints with Core Animation.

pub mod calendar;
pub mod demo;
pub mod draw;
pub mod layout;
pub mod media;
pub mod stats;
pub mod timer;
pub mod weather;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// The two views of the expanded notch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum View {
    /// Widgets.
    #[default]
    Home,
    /// The drop shelf.
    Shelf,
}

/// Which view the panel opens in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum DefaultView {
    /// The view that was showing last time (remembered across restarts).
    #[default]
    Last,
    Home,
    Shelf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum WidgetKind {
    NowPlaying,
    Calendar,
    Weather,
    Battery,
    SystemStats,
    Timer,
    QuickToggles,
    Note,
    Mirror,
    /// A kind written by a newer version; ignored (keeps old builds from
    /// rejecting the whole config).
    #[serde(other)]
    Unknown,
}

impl WidgetKind {
    pub const ALL: [WidgetKind; 9] = [
        WidgetKind::NowPlaying,
        WidgetKind::Calendar,
        WidgetKind::Weather,
        WidgetKind::Battery,
        WidgetKind::SystemStats,
        WidgetKind::Timer,
        WidgetKind::QuickToggles,
        WidgetKind::Note,
        WidgetKind::Mirror,
    ];

    pub fn title(self) -> &'static str {
        match self {
            WidgetKind::NowPlaying => "Now Playing",
            WidgetKind::Calendar => "Calendar",
            WidgetKind::Weather => "Weather",
            WidgetKind::Battery => "Battery",
            WidgetKind::SystemStats => "System Stats",
            WidgetKind::Timer => "Timer & Stopwatch",
            WidgetKind::QuickToggles => "Quick Controls",
            WidgetKind::Note => "Note",
            WidgetKind::Mirror => "Mirror",
            WidgetKind::Unknown => "Unknown",
        }
    }

    /// SF Symbol for settings lists.
    pub fn sf_symbol(self) -> &'static str {
        match self {
            WidgetKind::NowPlaying => "music.note",
            WidgetKind::Calendar => "calendar",
            WidgetKind::Weather => "cloud.sun.fill",
            WidgetKind::Battery => "battery.75percent",
            WidgetKind::SystemStats => "cpu",
            WidgetKind::Timer => "timer",
            WidgetKind::QuickToggles => "switch.2",
            WidgetKind::Note => "note.text",
            WidgetKind::Mirror => "person.crop.square",
            WidgetKind::Unknown => "questionmark",
        }
    }

    /// What enabling the widget involves (for the settings UI).
    pub fn note(self) -> Option<&'static str> {
        match self {
            WidgetKind::NowPlaying => Some("Uses the system Now Playing service; with \"Read Music and Spotify\" on, macOS asks once per app for Automation access."),
            WidgetKind::Calendar => Some("macOS asks for Calendar access the first time the widget is shown."),
            WidgetKind::Weather => Some("Fetches the forecast from open-meteo.com (no account) every 30 minutes."),
            WidgetKind::Mirror => Some("Shows the camera while you click it; macOS asks for Camera access. The camera stops when the notch closes."),
            _ => None,
        }
    }

    /// Default on/off.
    pub fn default_enabled(self) -> bool {
        matches!(self, WidgetKind::NowPlaying | WidgetKind::Battery | WidgetKind::Timer | WidgetKind::QuickToggles)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetConfig {
    pub kind: WidgetKind,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

impl WidgetConfig {
    /// Every widget in the default order with its default on/off state.
    pub fn defaults() -> Vec<WidgetConfig> {
        WidgetKind::ALL.iter().map(|&kind| WidgetConfig { kind, enabled: kind.default_enabled() }).collect()
    }

    /// The list as the settings UI should show it: the user's order (unknown
    /// kinds and duplicates dropped), then any kinds missing from it
    /// (added by a newer version), turned off.
    pub fn normalized(list: &[WidgetConfig]) -> Vec<WidgetConfig> {
        let mut out: Vec<WidgetConfig> = vec![];
        for w in list {
            if w.kind != WidgetKind::Unknown && !out.iter().any(|o| o.kind == w.kind) {
                out.push(*w);
            }
        }
        for k in WidgetKind::ALL {
            if !out.iter().any(|o| o.kind == k) {
                out.push(WidgetConfig { kind: k, enabled: false });
            }
        }
        out
    }

    /// The enabled kinds in display order.
    pub fn enabled_kinds(list: &[WidgetConfig]) -> Vec<WidgetKind> {
        Self::normalized(list).into_iter().filter(|w| w.enabled).map(|w| w.kind).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TempUnit {
    /// From the system's measurement settings.
    #[default]
    Auto,
    Celsius,
    Fahrenheit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct WeatherConfig {
    /// A city name ("Berlin", "Portland, Oregon") looked up with Open-Meteo's
    /// geocoding API, or "lat, lon" ("52.52, 13.41"). Empty: not set.
    pub location: String,
    pub units: TempUnit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarConfig {
    /// How many days to look ahead (1 = today only, 2 = today and tomorrow).
    pub days: u32,
    /// Calendar titles to include; empty = all calendars.
    pub calendars: Vec<String>,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        CalendarConfig { days: 2, calendars: vec![] }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct NowPlayingConfig {
    /// Read the current track from Music and Spotify with AppleScript when
    /// the system Now Playing service gives no data (macOS 15.4+ restricts
    /// it). macOS asks once per app for Automation permission. Off by
    /// default; without it the widget still has play/pause/next/previous
    /// (as media keys) but no title or artwork.
    pub use_applescript: bool,
}

// ----- host-provided controls -------------------------------------------

/// An on/off control provided by the host app (e.g. "Awake").
///
/// `get` and `set` are called on the main thread; they must be quick and
/// must not wait for the main thread themselves. `set` is invoked
/// asynchronously after the click has been handled.
#[derive(Clone)]
pub struct QuickToggle {
    pub id: String,
    pub title: String,
    pub sf_symbol: String,
    pub get: Arc<dyn Fn() -> bool + Send + Sync>,
    pub set: Arc<dyn Fn(bool) + Send + Sync>,
}

/// A one-shot action provided by the host app (e.g. "Pick color"). The
/// panel collapses first, then `run` is called on the main thread.
#[derive(Clone)]
pub struct QuickAction {
    pub id: String,
    pub title: String,
    pub sf_symbol: String,
    pub run: Arc<dyn Fn() + Send + Sync>,
}

impl QuickToggle {
    pub fn new(id: &str, title: &str, sf_symbol: &str, get: impl Fn() -> bool + Send + Sync + 'static, set: impl Fn(bool) + Send + Sync + 'static) -> Self {
        QuickToggle { id: id.into(), title: title.into(), sf_symbol: sf_symbol.into(), get: Arc::new(get), set: Arc::new(set) }
    }
}

impl QuickAction {
    pub fn new(id: &str, title: &str, sf_symbol: &str, run: impl Fn() + Send + Sync + 'static) -> Self {
        QuickAction { id: id.into(), title: title.into(), sf_symbol: sf_symbol.into(), run: Arc::new(run) }
    }
}

impl std::fmt::Debug for QuickToggle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuickToggle").field("id", &self.id).field("title", &self.title).finish()
    }
}

impl std::fmt::Debug for QuickAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuickAction").field("id", &self.id).field("title", &self.title).finish()
    }
}

/// Extra things the host hands to [`crate::NotchShelf::start_with`].
#[derive(Clone, Debug, Default)]
pub struct StartOptions {
    /// Shown in the Quick Controls widget, toggles first, in this order.
    pub toggles: Vec<QuickToggle>,
    pub actions: Vec<QuickAction>,
}

// ----- data the providers deliver ---------------------------------------

/// sRGB color with alpha, 0..1.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rgba(pub f64, pub f64, pub f64, pub f64);

impl Rgba {
    pub const fn white(a: f64) -> Rgba {
        Rgba(1.0, 1.0, 1.0, a)
    }
    pub fn with_alpha(self, a: f64) -> Rgba {
        Rgba(self.0, self.1, self.2, a)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MediaSource {
    /// No metadata; controls are sent as media keys.
    #[default]
    MediaKeys,
    /// The system Now Playing service (MediaRemote).
    System,
    Music,
    Spotify,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Player app name, if known ("Music", "Spotify", "Safari").
    pub app: String,
    pub playing: bool,
    /// Seconds.
    pub duration: Option<f64>,
    /// Seconds into the track at `sampled_at`.
    pub elapsed: Option<f64>,
    /// Unix time (seconds) of `elapsed`.
    pub sampled_at: f64,
    /// Encoded image (PNG/JPEG/...), shared so snapshots stay cheap.
    pub artwork: Option<Arc<Vec<u8>>>,
    pub source: MediaSource,
}

impl NowPlaying {
    pub fn has_track(&self) -> bool {
        !self.title.trim().is_empty()
    }

    /// Key that changes when the artwork does.
    pub fn artwork_key(&self) -> u64 {
        self.artwork.as_ref().map(|a| {
            // FNV-1a over the length and a sample of the bytes.
            let mut h: u64 = 0xcbf29ce484222325;
            let step = (a.len() / 256).max(1);
            for b in a.len().to_le_bytes().iter().chain(a.iter().step_by(step)) {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            h
        }).unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct CalEvent {
    /// EventKit event identifier (to open it in Calendar).
    pub id: String,
    pub title: String,
    /// Unix seconds.
    pub start: f64,
    pub end: f64,
    pub all_day: bool,
    pub color: Rgba,
    pub calendar: String,
    pub location: String,
    /// Localized start time ("10:30", "10:30 AM"), from the platform.
    pub time_label: String,
}

/// Permission-gated features.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Access {
    #[default]
    Unknown,
    /// Not asked yet.
    NotDetermined,
    Requesting,
    Granted,
    Denied,
    /// The host app's Info.plist lacks the usage-description key, so the
    /// system would refuse (or kill the app); never asked.
    MissingUsageKey,
    /// Not available on this platform / machine.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WeatherNow {
    pub place: String,
    pub temp: f64,
    pub high: f64,
    pub low: f64,
    pub code: u32,
    pub is_day: bool,
    pub fahrenheit: bool,
    /// Unix seconds.
    pub fetched_at: f64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum WeatherStatus {
    #[default]
    Off,
    NoLocation,
    Loading,
    Ready(WeatherNow),
    /// Fetch / geocoding failure (message), possibly with stale data.
    Error(String, Option<WeatherNow>),
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct BatteryInfo {
    pub percent: u8,
    pub on_ac: bool,
    pub charging: bool,
    /// Minutes until empty (on battery) / full (charging), when the system
    /// has an estimate.
    pub minutes_to_empty: Option<u32>,
    pub minutes_to_full: Option<u32>,
}

/// Everything the providers deliver, as one snapshot.
#[derive(Clone, Debug, Default)]
pub struct WidgetData {
    pub now_playing: Option<NowPlaying>,
    pub calendar_access: Access,
    pub events: Vec<CalEvent>,
    pub weather: WeatherStatus,
    pub battery: Option<BatteryInfo>,
    pub stats: stats::StatsView,
    pub camera_access: Access,
}

/// Permission states for the host's settings page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WidgetStatus {
    pub calendar: Access,
    pub camera: Access,
    pub media_source: Option<MediaSource>,
}

/// "3:07" / "1:02:03".
pub fn format_clock(secs: f64) -> String {
    let s = secs.max(0.0).floor() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Unix time now, in seconds.
pub fn unix_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_list_normalization() {
        let d = WidgetConfig::defaults();
        assert_eq!(d.len(), WidgetKind::ALL.len());
        assert_eq!(WidgetConfig::enabled_kinds(&d), vec![WidgetKind::NowPlaying, WidgetKind::Battery, WidgetKind::Timer, WidgetKind::QuickToggles]);
        let user = vec![
            WidgetConfig { kind: WidgetKind::Timer, enabled: true },
            WidgetConfig { kind: WidgetKind::Unknown, enabled: true },
            WidgetConfig { kind: WidgetKind::Weather, enabled: true },
            WidgetConfig { kind: WidgetKind::Timer, enabled: false },
        ];
        let n = WidgetConfig::normalized(&user);
        assert_eq!(n.len(), WidgetKind::ALL.len());
        assert_eq!(n[0], WidgetConfig { kind: WidgetKind::Timer, enabled: true });
        assert_eq!(n[1].kind, WidgetKind::Weather);
        assert!(n[2..].iter().all(|w| !w.enabled));
        assert_eq!(WidgetConfig::enabled_kinds(&user), vec![WidgetKind::Timer, WidgetKind::Weather]);
    }

    #[test]
    fn widget_kind_serde() {
        let v: Vec<WidgetConfig> = serde_json::from_str(r#"[{"kind":"nowPlaying","enabled":false},{"kind":"holograms","enabled":true},{"kind":"systemStats"}]"#).unwrap();
        assert_eq!(v[0], WidgetConfig { kind: WidgetKind::NowPlaying, enabled: false });
        assert_eq!(v[1].kind, WidgetKind::Unknown);
        assert!(v[2].enabled);
        assert_eq!(serde_json::to_string(&WidgetKind::QuickToggles).unwrap(), "\"quickToggles\"");
    }

    #[test]
    fn clock_format() {
        assert_eq!(format_clock(0.0), "0:00");
        assert_eq!(format_clock(65.9), "1:05");
        assert_eq!(format_clock(3723.0), "1:02:03");
        assert_eq!(format_clock(-4.0), "0:00");
    }

    #[test]
    fn artwork_key_changes_with_content() {
        let a = NowPlaying { artwork: Some(Arc::new(vec![1, 2, 3])), ..Default::default() };
        let b = NowPlaying { artwork: Some(Arc::new(vec![1, 2, 4])), ..Default::default() };
        assert_ne!(a.artwork_key(), b.artwork_key());
        assert_eq!(NowPlaying::default().artwork_key(), 0);
    }
}
