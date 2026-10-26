//! The Notch Widgets page: the Home view of the Notch Shelf as a widget
//! gallery and editor.
//!
//! * **Pages**: named Home pages with an optional icon; add, rename,
//!   reorder, delete (widgets of a deleted page move to its neighbour).
//! * **Widgets on a page**: the instances in order, with a to-scale preview
//!   of the notch, drag / arrow reordering, size, on/off, duplicate, move to
//!   another page and delete.
//! * **Gallery**: every kind of [`notch_shelf::widget_catalog`] by category;
//!   a click adds one with its default settings.
//! * **Editor**: a form generated from the kind's settings schema
//!   ([`notch_shelf::SettingField`]) with inline validation
//!   ([`notch_shelf::validate_settings`]), plus helpers: the Shortcuts
//!   picker, an app menu for the Launcher, time zones for World Clocks, the
//!   xbar plugin folder and a "Test" run for Script / Web / xbar.
//! * **Share**: layout export / import.
//!
//! Everything edits `config.json` (auto-saved like every page); the running
//! app applies notch changes live. Commands and paths of Script / xbar /
//! Launcher widgets are only committed when their field loses focus, so a
//! half-typed command is never run. The running app reports widget results
//! and reminder lists through `notch-status.json`.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use notch_shelf::widgets::catalog::Category;
use notch_shelf::{FieldError, FieldType, ImportMode, SettingField, WidgetInstance, WidgetKind, WidgetKindInfo, WidgetPage, WidgetSize};
use pm_config::bridge::{NotchRequest, WidgetPreview};
use serde_json::{json, Map, Value};

use super::pages::callout;
use super::*;

/// A "Test" run of a Script / xbar / Web widget.
#[derive(Clone)]
pub(crate) enum TestRun {
    Running(Instant),
    Done { result: Box<WidgetPreview>, took: Duration, at: Instant },
}

/// An xbar folder listing: (folder, when read, plugins as (file, interval)).
type XbarCache = (String, Instant, Vec<(String, String)>);

/// Results of file pickers and other background lookups, by key.
type Picks = Arc<Mutex<Vec<(String, Result<String, String>)>>>;

/// UI state of the Notch Widgets page.
pub(crate) struct WidgetsUi {
    /// The Home page being edited.
    page: usize,
    /// The instance whose settings are open.
    selected: Option<String>,
    gallery: bool,
    gallery_query: String,
    /// Secret fields shown in clear ("<id>/<field>").
    secrets_shown: HashSet<String>,
    /// `shortcuts list`, loaded on another thread when first needed.
    shortcuts: Arc<Mutex<Option<Vec<String>>>>,
    shortcuts_requested: bool,
    tests: Arc<Mutex<BTreeMap<String, TestRun>>>,
    picks: Picks,
    choosing: HashSet<String>,
    /// Installed apps (Launcher), read when first needed.
    apps: Option<Vec<String>>,
    /// xbar plugin folder listing: (folder, when read, plugins).
    xbar_cache: Option<XbarCache>,
    export_path: String,
    export_secrets: bool,
    import_path: String,
    import_append: bool,
    /// Outcome of the last export / import: (ok, message, warnings).
    io_result: Option<(bool, String, Vec<String>)>,
    /// The layout before the last import (for "Undo import").
    undo: Option<(Vec<WidgetPage>, Vec<WidgetInstance>)>,
    /// A delete waiting for confirmation (instance id or "page:<n>").
    confirm: Option<String>,
    /// A widget was just added: scroll its editor into view.
    scroll_to_editor: bool,
}

impl Default for WidgetsUi {
    fn default() -> Self {
        let downloads = home_dir().map(|h| h.join("Downloads").join("notch-layout.json"));
        WidgetsUi {
            page: 0,
            selected: None,
            gallery: false,
            gallery_query: String::new(),
            secrets_shown: HashSet::new(),
            shortcuts: Arc::new(Mutex::new(None)),
            shortcuts_requested: false,
            tests: Default::default(),
            picks: Default::default(),
            choosing: HashSet::new(),
            apps: None,
            xbar_cache: None,
            export_path: downloads.map(|p| tilde(&p.to_string_lossy())).unwrap_or_default(),
            export_secrets: false,
            import_path: String::new(),
            import_append: false,
            io_result: None,
            undo: None,
            confirm: None,
            scroll_to_editor: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Look

/// Accent color of a widget kind (its icon tile).
fn kind_tint(k: WidgetKind) -> Color32 {
    let c = |r, g, b| Color32::from_rgb(r, g, b);
    match k {
        WidgetKind::NowPlaying => c(0xFF, 0x2D, 0x55),
        WidgetKind::Calendar => c(0xF0, 0x3E, 0x36),
        WidgetKind::Weather => c(0x2F, 0x9B, 0xE8),
        WidgetKind::Battery => c(0x30, 0xB8, 0x50),
        WidgetKind::SystemStats => c(0x58, 0x56, 0xD6),
        WidgetKind::Timer => c(0xFF, 0x95, 0x00),
        WidgetKind::QuickToggles => c(0x0A, 0x7A, 0xF5),
        WidgetKind::Note => c(0xE8, 0xAC, 0x00),
        WidgetKind::Mirror => c(0x7C, 0x7C, 0x84),
        WidgetKind::Script => c(0x4A, 0x52, 0x60),
        WidgetKind::Xbar => c(0xAF, 0x52, 0xDE),
        WidgetKind::Web => c(0x14, 0x8E, 0xD8),
        WidgetKind::Shortcuts => c(0xE5, 0x45, 0x7E),
        WidgetKind::Launcher => c(0x5E, 0x5C, 0xE6),
        WidgetKind::Bluetooth => c(0x1B, 0x6E, 0xF3),
        WidgetKind::Sound => c(0xFF, 0x37, 0x5F),
        WidgetKind::Reminders => c(0xF5, 0x7C, 0x00),
        WidgetKind::WorldClock => c(0x0E, 0x9F, 0x9A),
        WidgetKind::Unknown => c(0x8E, 0x8E, 0x93),
    }
}

/// Phosphor glyphs for the SF Symbols the widgets and pages use.
mod glyph {
    pub const BATTERY: &str = "\u{E0C2}";
    pub const BELL: &str = "\u{E0CE}";
    pub const BOOK: &str = "\u{E0E6}";
    pub const BRIEFCASE: &str = "\u{E0EE}";
    pub const CALENDAR: &str = "\u{E108}";
    pub const CAMERA: &str = "\u{E10E}";
    pub const CHART_BAR: &str = "\u{E150}";
    pub const CHECKS: &str = "\u{EADC}";
    pub const CLOUD_SUN: &str = "\u{E540}";
    pub const CODE: &str = "\u{E1BC}";
    pub const CPU: &str = "\u{E610}";
    pub const DESKTOP: &str = "\u{E560}";
    pub const DOTS_THREE: &str = "\u{E1FE}";
    pub const DOWNLOAD: &str = "\u{E20C}";
    pub const ENVELOPE: &str = "\u{E214}";
    pub const EXPORT: &str = "\u{EAF0}";
    pub const FILE: &str = "\u{E230}";
    pub const FIRE: &str = "\u{E242}";
    pub const FLASK: &str = "\u{E79E}";
    pub const FOLDER: &str = "\u{E24A}";
    pub const GAME: &str = "\u{E26E}";
    pub const GLOBE: &str = "\u{E28C}";
    pub const HARD_DRIVE: &str = "\u{E29E}";
    pub const HEADPHONES: &str = "\u{E2A6}";
    pub const HEART: &str = "\u{E2A8}";
    pub const HOUSE: &str = "\u{E2C2}";
    pub const LEAF: &str = "\u{E2DA}";
    pub const LINK: &str = "\u{E2E2}";
    pub const LIST: &str = "\u{E2F0}";
    pub const MOON: &str = "\u{E330}";
    pub const MUSIC: &str = "\u{E33C}";
    pub const NOTE: &str = "\u{E348}";
    pub const PUZZLE: &str = "\u{E596}";
    pub const QUESTION: &str = "\u{E3E8}";
    pub const ROCKET: &str = "\u{E3FC}";
    pub const SQUARE: &str = "\u{E45E}";
    pub const STACK: &str = "\u{E466}";
    pub const STAR: &str = "\u{E46A}";
    pub const TERMINAL: &str = "\u{EAE8}";
    pub const TOGGLE: &str = "\u{E676}";
    pub const USER: &str = "\u{E4C2}";
    pub const USER_SQUARE: &str = "\u{E4D4}";
    pub const WIFI: &str = "\u{E4EA}";
    pub const AIRPLANE: &str = "\u{E002}";
    pub const COINS: &str = "\u{E78E}";
    pub const WRENCH: &str = "\u{E5D4}";
    pub const PLUS_CIRCLE: &str = "\u{E3D6}";
}

/// A Phosphor glyph for an SF Symbol name (approximate; unknown names get a
/// neutral square).
pub(crate) fn sf_glyph(name: &str) -> &'static str {
    let n = name.trim();
    let base = n.strip_suffix(".fill").unwrap_or(n);
    match base {
        "music.note" | "music.note.list" | "music.quarternote.3" => glyph::MUSIC,
        "calendar" | "calendar.badge.clock" => glyph::CALENDAR,
        "cloud.sun" | "sun.max" | "cloud" | "cloud.rain" => glyph::CLOUD_SUN,
        "battery.75percent" | "battery.100" | "battery.50" | "battery.25" | "bolt.batteryblock" => glyph::BATTERY,
        "cpu" | "memorychip" => glyph::CPU,
        "timer" | "stopwatch" | "clock" | "alarm" => icon::TIMER,
        "switch.2" | "slider.horizontal.3" | "togglepower" => glyph::TOGGLE,
        "note.text" | "note" | "doc.text" | "square.and.pencil" => glyph::NOTE,
        "person.crop.square" | "camera" | "video" => glyph::USER_SQUARE,
        "terminal" | "apple.terminal" | "chevron.left.forwardslash.chevron.right" => glyph::TERMINAL,
        "puzzlepiece.extension" | "puzzlepiece" => glyph::PUZZLE,
        "network" | "server.rack" => icon::NETWORK,
        "square.2.layers.3d" | "square.stack" | "rectangle.stack" | "square.3.layers.3d" => glyph::STACK,
        "square.grid.2x2" | "square.grid.3x3" | "app.badge" => icon::SQUARES_FOUR,
        "airpodspro" | "airpods" | "airpodsmax" | "headphones" | "beats.headphones" => glyph::HEADPHONES,
        "speaker.wave.2" | "speaker.wave.3" | "speaker.wave.1" | "hifispeaker" | "speaker" => icon::SPEAKER_HIGH,
        "speaker.slash" => icon::SPEAKER_X,
        "checklist" | "checkmark.circle" | "list.bullet.circle" => glyph::CHECKS,
        "globe" | "globe.americas" | "globe.europe.africa" | "globe.asia.australia" => glyph::GLOBE,
        "house" => glyph::HOUSE,
        "briefcase" | "bag" => glyph::BRIEFCASE,
        "star" => glyph::STAR,
        "heart" => glyph::HEART,
        "bolt" | "bolt.circle" => icon::LIGHTNING,
        "chevron.left.slash.chevron.right" | "curlybraces" => glyph::CODE,
        "gamecontroller" => glyph::GAME,
        "chart.bar" | "chart.xyaxis.line" | "chart.line.uptrend.xyaxis" => glyph::CHART_BAR,
        "book" | "books.vertical" => glyph::BOOK,
        "cup.and.saucer" | "mug" => icon::COFFEE,
        "moon" | "moon.stars" => glyph::MOON,
        "leaf" => glyph::LEAF,
        "flame" => glyph::FIRE,
        "desktopcomputer" | "display" | "tv" => glyph::DESKTOP,
        "laptopcomputer" | "macbook" => icon::LAPTOP,
        "internaldrive" | "externaldrive" => glyph::HARD_DRIVE,
        "wifi" => glyph::WIFI,
        "bell" => glyph::BELL,
        "envelope" => glyph::ENVELOPE,
        "person" | "person.circle" => glyph::USER,
        "airplane" => glyph::AIRPLANE,
        "bitcoinsign.circle" | "dollarsign.circle" | "eurosign.circle" => glyph::COINS,
        "wrench.and.screwdriver" | "wrench" | "hammer" => glyph::WRENCH,
        "folder" => glyph::FOLDER,
        "doc" => glyph::FILE,
        "link" => glyph::LINK,
        "list.bullet" => glyph::LIST,
        "flask" | "testtube.2" => glyph::FLASK,
        "paperplane" | "rocket" => glyph::ROCKET,
        "eyedropper" => icon::EYEDROPPER,
        "lock" => icon::LOCK_KEY,
        "questionmark" => glyph::QUESTION,
        "camera.viewfinder" => glyph::CAMERA,
        _ => glyph::SQUARE,
    }
}

/// Icons a page can have (SF Symbol names, shown in the notch's tabs).
const PAGE_SYMBOLS: &[(&str, &str)] = &[
    ("", "None (show the name)"),
    ("house.fill", "Home"),
    ("briefcase.fill", "Work"),
    ("chevron.left.forwardslash.chevron.right", "Code"),
    ("music.note", "Music"),
    ("chart.bar.fill", "Stats"),
    ("calendar", "Calendar"),
    ("checklist", "Tasks"),
    ("globe", "World"),
    ("star.fill", "Favorites"),
    ("heart.fill", "Personal"),
    ("bolt.fill", "Quick"),
    ("gamecontroller.fill", "Games"),
    ("book.fill", "Reading"),
    ("cup.and.saucer.fill", "Break"),
    ("moon.fill", "Night"),
    ("leaf.fill", "Nature"),
    ("flame.fill", "Hot"),
    ("desktopcomputer", "Desktop"),
    ("internaldrive.fill", "Storage"),
    ("wifi", "Network"),
    ("bell.fill", "Alerts"),
    ("envelope.fill", "Mail"),
    ("person.fill", "People"),
    ("airplane", "Travel"),
    ("bitcoinsign.circle.fill", "Finance"),
    ("wrench.and.screwdriver.fill", "Tools"),
    ("terminal.fill", "Terminal"),
];

/// Ids of PowerMac's quick controls (for the Quick Controls widget).
const QUICK_CONTROLS: &[(&str, &str)] = &[
    ("awake", "Awake"),
    ("pick-color", "Pick color"),
    ("extract-text", "Extract text"),
    ("clipboard", "Clipboard"),
    ("switcher", "Windows"),
    ("lock", "Lock screen"),
    ("desktop", "Show desktop"),
];

/// Time zones offered in the World Clocks menu (any IANA name works).
const TIME_ZONES: &[(&str, &str)] = &[
    ("Pacific/Honolulu", "Honolulu"),
    ("America/Anchorage", "Anchorage"),
    ("America/Los_Angeles", "Cupertino"),
    ("America/Denver", "Denver"),
    ("America/Chicago", "Chicago"),
    ("America/New_York", "New York"),
    ("America/Toronto", "Toronto"),
    ("America/Mexico_City", "Mexico City"),
    ("America/Bogota", "Bogotá"),
    ("America/Sao_Paulo", "São Paulo"),
    ("America/Argentina/Buenos_Aires", "Buenos Aires"),
    ("Atlantic/Reykjavik", "Reykjavík"),
    ("Europe/London", "London"),
    ("Europe/Dublin", "Dublin"),
    ("Europe/Lisbon", "Lisbon"),
    ("Europe/Paris", "Paris"),
    ("Europe/Berlin", "Berlin"),
    ("Europe/Madrid", "Madrid"),
    ("Europe/Rome", "Rome"),
    ("Europe/Amsterdam", "Amsterdam"),
    ("Europe/Stockholm", "Stockholm"),
    ("Europe/Warsaw", "Warsaw"),
    ("Europe/Athens", "Athens"),
    ("Europe/Istanbul", "Istanbul"),
    ("Europe/Kyiv", "Kyiv"),
    ("Europe/Moscow", "Moscow"),
    ("Africa/Cairo", "Cairo"),
    ("Africa/Lagos", "Lagos"),
    ("Africa/Johannesburg", "Johannesburg"),
    ("Asia/Dubai", "Dubai"),
    ("Asia/Karachi", "Karachi"),
    ("Asia/Kolkata", "Bengaluru"),
    ("Asia/Bangkok", "Bangkok"),
    ("Asia/Singapore", "Singapore"),
    ("Asia/Hong_Kong", "Hong Kong"),
    ("Asia/Shanghai", "Shanghai"),
    ("Asia/Taipei", "Taipei"),
    ("Asia/Seoul", "Seoul"),
    ("Asia/Tokyo", "Tokyo"),
    ("Australia/Perth", "Perth"),
    ("Australia/Sydney", "Sydney"),
    ("Pacific/Auckland", "Auckland"),
    ("UTC", "UTC"),
];

fn category_name(c: Category) -> &'static str {
    match c {
        Category::BuildYourOwn => "Build your own",
        Category::Information => "Information",
        Category::Media => "Media",
        Category::System => "System",
        Category::Tools => "Tools",
    }
}

fn category_blurb(c: Category) -> &'static str {
    match c {
        Category::BuildYourOwn => "Show anything: your scripts, xbar plugins, web APIs, Shortcuts and launchers.",
        Category::Information => "Calendar, weather, reminders and the time elsewhere.",
        Category::Media => "What's playing and where the sound goes.",
        Category::System => "Batteries and how busy this Mac is.",
        Category::Tools => "Timers, notes, quick controls and the mirror.",
    }
}

const CATEGORIES: [Category; 5] = [Category::BuildYourOwn, Category::Information, Category::Media, Category::System, Category::Tools];

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// `~/x` for paths in the home folder.
fn tilde(p: &str) -> String {
    match home_dir() {
        Some(h) => {
            let h = h.to_string_lossy().to_string();
            match p.strip_prefix(&h) {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
                _ => p.to_string(),
            }
        }
        None => p.to_string(),
    }
}

/// `~/x` → `/Users/me/x`.
fn expand(p: &str) -> std::path::PathBuf {
    let p = p.trim();
    match (p.strip_prefix("~/"), home_dir()) {
        (Some(rest), Some(h)) => h.join(rest),
        _ if p == "~" => home_dir().unwrap_or_default(),
        _ => std::path::PathBuf::from(p),
    }
}

/// The settings object of `w` (made an object if it isn't one).
fn obj_mut(w: &mut WidgetInstance) -> &mut Map<String, Value> {
    if !w.settings.is_object() {
        w.settings = notch_shelf::normalize_settings(w.kind, &w.settings);
    }
    w.settings.as_object_mut().expect("object")
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// The card name of an instance: its title setting, else the kind's title.
pub(crate) fn instance_title(w: &WidgetInstance) -> String {
    let t = text_of(w.settings.get("title"));
    if !t.trim().is_empty() && !(w.kind == WidgetKind::Shortcuts && t == "Shortcuts") {
        return t.trim().to_string();
    }
    match w.kind {
        WidgetKind::Unknown => "Unknown widget".into(),
        k => notch_shelf::kind_info(k).title.to_string(),
    }
}

/// One line about what the instance shows.
fn instance_summary(w: &WidgetInstance) -> String {
    let s = &w.settings;
    let list_len = |k: &str| s.get(k).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    let first_line = |k: &str| text_of(s.get(k)).lines().next().unwrap_or("").trim().to_string();
    let short = |t: String, n: usize| if t.chars().count() > n { format!("{}…", t.chars().take(n).collect::<String>()) } else { t };
    match w.kind {
        WidgetKind::Script => {
            let c = first_line("command");
            if c.is_empty() { "No command yet".into() } else { short(c, 48) }
        }
        WidgetKind::Xbar => {
            let p = text_of(s.get("path"));
            let name = p.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
            if p.trim().is_empty() { "No plugin chosen".into() } else { short(name, 48) }
        }
        WidgetKind::Web => {
            let u = text_of(s.get("url"));
            let host = u.split("://").nth(1).unwrap_or(&u).split('/').next().unwrap_or("").to_string();
            if host.is_empty() { "No URL yet".into() } else { host }
        }
        WidgetKind::Shortcuts => {
            let names: Vec<String> = s.get("shortcuts").and_then(|v| v.as_array()).map(|a| a.iter().map(|x| text_of(Some(x))).collect()).unwrap_or_default();
            if names.is_empty() { "No shortcuts chosen".into() } else { short(names.join(", "), 48) }
        }
        WidgetKind::Launcher => plural(list_len("items"), "item"),
        WidgetKind::WorldClock => {
            let labels: Vec<String> = s
                .get("clocks")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|c| {
                            let l = text_of(c.get("label"));
                            if l.is_empty() { text_of(c.get("zone")).rsplit('/').next().unwrap_or("").replace('_', " ") } else { l }
                        })
                        .collect()
                })
                .unwrap_or_default();
            if labels.is_empty() { "No clocks".into() } else { short(labels.join(" · "), 48) }
        }
        WidgetKind::Weather => {
            let l = text_of(s.get("location"));
            if l.trim().is_empty() { "Your location".into() } else { l }
        }
        WidgetKind::Calendar => {
            let d = s.get("days").and_then(|v| v.as_f64()).unwrap_or(2.0) as usize;
            format!("Next {}", plural(d, "day"))
        }
        WidgetKind::Timer => {
            let p: Vec<String> = s.get("presets").and_then(|v| v.as_array()).map(|a| a.iter().map(|x| text_of(Some(x))).collect()).unwrap_or_default();
            if p.is_empty() { "Stopwatch".into() } else { format!("{} min", p.join(" / ")) }
        }
        WidgetKind::Reminders => {
            let n = list_len("lists");
            if n == 0 { "All lists".into() } else { plural(n, "list") }
        }
        WidgetKind::Bluetooth => {
            let n = list_len("devices");
            if n == 0 { "Every connected device".into() } else { plural(n, "device") }
        }
        WidgetKind::QuickToggles => {
            let n = list_len("controls");
            if n == 0 { "All controls".into() } else { plural(n, "control") }
        }
        WidgetKind::Unknown => "From a newer version of PowerMac; kept but not shown".into(),
        k => notch_shelf::kind_info(k).description.to_string(),
    }
}

fn size_letter(s: WidgetSize) -> &'static str {
    match s {
        WidgetSize::Small => "S",
        WidgetSize::Medium => "M",
        WidgetSize::Large => "L",
    }
}

/// Whether field `f` shows given the object's other values (`show_if`).
fn field_shown(f: &SettingField, fields: &[SettingField], obj: &Map<String, Value>) -> bool {
    let Some(cond) = &f.show_if else { return true };
    let v = obj.get(cond.field).cloned().or_else(|| fields.iter().find(|x| x.name == cond.field).map(|x| x.default.clone())).unwrap_or(Value::Null);
    let as_text = text_of(Some(&v));
    cond.values.iter().any(|want| *want == v || want.as_str().is_some_and(|w| w == as_text))
}

/// Errors that belong to the field at `path` (itself or inside it).
fn errors_for<'a>(errors: &'a [FieldError], path: &str) -> Vec<&'a FieldError> {
    errors
        .iter()
        .filter(|e| e.field == path || e.field.starts_with(&format!("{path}[")) || e.field.starts_with(&format!("{path}.")))
        .collect()
}

/// "clocks[1].zone" → "Clock 2 · Time zone".
fn error_text(f: &SettingField, e: &FieldError, path: &str) -> String {
    let rest = e.field.strip_prefix(path).unwrap_or("");
    let msg = &e.message;
    if rest.is_empty() {
        return format!("{} {msg}", f.label);
    }
    let mut idx = None;
    let mut sub = None;
    if let Some(r) = rest.strip_prefix('[') {
        if let Some((n, tail)) = r.split_once(']') {
            idx = n.parse::<usize>().ok();
            sub = tail.strip_prefix('.').map(|s| s.to_string());
        }
    }
    let sub_label = sub.as_deref().and_then(|s| f.item_fields.iter().find(|x| x.name == s)).map(|x| x.label);
    match (idx, sub_label) {
        (Some(i), Some(l)) => format!("{} {}: {l} {msg}", item_noun(f), i + 1),
        (Some(i), None) => format!("{} {}: {msg}", item_noun(f), i + 1),
        _ => format!("{}: {msg}", f.label),
    }
}

/// "Clock", "Item", "Entry" for a list field.
fn item_noun(f: &SettingField) -> &'static str {
    match f.name {
        "clocks" => "Clock",
        "items" => "Item",
        "shortcuts" => "Shortcut",
        "presets" => "Preset",
        _ => "Entry",
    }
}

/// Integral numbers stay integers in JSON.
fn num_value(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9e15 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

/// Fields whose text is only committed when the field loses focus (they
/// run something; a half-typed command must never run).
fn commit_on_blur(kind: WidgetKind, f: &SettingField) -> bool {
    matches!(f.ty, FieldType::Path | FieldType::Url)
        || (kind == WidgetKind::Script && matches!(f.name, "command" | "clickTarget"))
        || (kind == WidgetKind::Launcher && f.name == "target")
        || (kind == WidgetKind::Web && matches!(f.name, "template" | "valuePath" | "listPath" | "itemTemplate"))
}

/// How a text field looks.
#[derive(Clone, Copy)]
struct TextOpts<'a> {
    hint: &'a str,
    width: f32,
    mono: bool,
    /// A code area with this many rows (0: one line).
    rows: usize,
}

fn text_edit<'t>(buf: &'t mut String, o: TextOpts<'_>) -> egui::TextEdit<'t> {
    let mut te = if o.rows > 0 { egui::TextEdit::multiline(buf).code_editor().desired_rows(o.rows) } else { egui::TextEdit::singleline(buf) };
    te = te.hint_text(o.hint.to_string()).desired_width(o.width);
    if o.mono && o.rows == 0 {
        te = te.font(egui::TextStyle::Monospace);
    }
    te
}

/// A text field that edits a buffer and commits it to `value` when it
/// loses focus (Return in a one-line field). Returns true when committed.
fn deferred_text(ui: &mut Ui, id: Id, value: &mut String, o: TextOpts<'_>) -> (bool, egui::Response) {
    let key = id.with("deferred");
    let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| value.clone());
    let r = ui.add(text_edit(&mut buf, o).id(id));
    let mut committed = false;
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(key, buf));
    } else {
        if r.lost_focus() && buf != *value {
            *value = buf;
            committed = true;
        }
        ui.data_mut(|d| d.remove::<String>(key));
    }
    (committed, r)
}

/// Whether a deferred field has typing that isn't applied yet.
fn deferred_dirty(ui: &Ui, id: Id, value: &str) -> bool {
    ui.data(|d| d.get_temp::<String>(id.with("deferred"))).is_some_and(|b| b != value)
}

/// A native file / folder dialog (macOS, through AppleScript): the POSIX
/// path, or "" when cancelled.
fn choose_path(what: &'static str, prompt: String) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let p = prompt.replace('"', "'");
        let script = match what {
            "folder" => format!("POSIX path of (choose folder with prompt \"{p}\")"),
            "save" => format!("POSIX path of (choose file name with prompt \"{p}\" default name \"notch-layout.json\")"),
            "app" => format!("POSIX path of (choose application with prompt \"{p}\" as alias)"),
            _ => format!("POSIX path of (choose file with prompt \"{p}\")"),
        };
        let out = std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).output().map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (what, prompt);
        Ok(String::new())
    }
}

/// Apps in /Applications, ~/Applications and /System/Applications (names).
fn installed_apps() -> Vec<String> {
    let mut dirs = vec![std::path::PathBuf::from("/Applications"), std::path::PathBuf::from("/System/Applications"), std::path::PathBuf::from("/System/Applications/Utilities"), std::path::PathBuf::from("/Applications/Utilities")];
    if let Some(h) = home_dir() {
        dirs.push(h.join("Applications"));
    }
    let mut v: Vec<String> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".app")).map(String::from))
        .collect();
    v.sort_by_key(|a| a.to_lowercase());
    v.dedup();
    v
}

/// Plugins in an xbar folder (or the one file): (file name, refresh text).
fn xbar_plugins(path: &str) -> Vec<(String, String)> {
    let p = expand(path);
    let files: Vec<std::path::PathBuf> = if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(&p).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        v.sort();
        v
    } else if p.is_file() {
        vec![p]
    } else {
        vec![]
    };
    files
        .into_iter()
        .filter_map(|f| {
            let name = f.file_name()?.to_str()?.to_string();
            if name.starts_with('.') || f.is_dir() {
                return None;
            }
            Some((name.clone(), xbar_interval_text(&name)))
        })
        .take(30)
        .collect()
}

/// "every 5 min" from "cpu.5m.sh".
fn xbar_interval_text(name: &str) -> String {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() >= 3 {
        let t = parts[parts.len() - 2];
        let (num, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
        if let Ok(n) = num.parse::<u64>() {
            let u = match unit {
                "ms" => "ms",
                "s" => "s",
                "m" => "min",
                "h" => "h",
                "d" => "d",
                _ => "",
            };
            if !u.is_empty() {
                return format!("every {n} {u}");
            }
        }
    }
    "no interval in the name".into()
}

// ---------------------------------------------------------------------------
// The page

impl SettingsApp {
    pub(super) fn notch_widgets_page(&mut self, ui: &mut Ui) {
        self.hero(ui, |_, _| {});
        self.collect_widget_picks(ui.ctx());
        let n_enabled = self.app.cfg.notch_shelf.enabled;
        if !cfg!(target_os = "macos") {
            callout(ui, icon::INFO, "Widgets are a macOS feature", "On Windows the shelf has no Home view. You can still prepare and share layouts here.");
        } else if !n_enabled {
            let mut on = false;
            notice_with_switch(ui, "The Notch Shelf is off", "Widgets show in the open notch. Turn the shelf on to see them.", &mut on);
            if on {
                self.app.cfg.notch_shelf.enabled = true;
            }
        }
        // Keep the selection valid (the file may have changed elsewhere).
        let pages_n = self.app.cfg.notch_shelf.page_list().len();
        self.nw.page = self.nw.page.min(pages_n - 1);
        if let Some(id) = &self.nw.selected {
            if !self.app.cfg.notch_shelf.widgets.iter().any(|w| &w.id == id) {
                self.nw.selected = None;
            }
        }
        self.pages_card(ui);
        self.widgets_card(ui);
        if self.nw.gallery {
            self.gallery_card(ui);
        }
        self.editor_card(ui);
        self.home_options_card(ui);
        self.share_card(ui);
    }

    /// Results of file pickers / lookups that finished on other threads.
    fn collect_widget_picks(&mut self, ctx: &egui::Context) {
        let done: Vec<_> = std::mem::take(&mut *self.nw.picks.lock().unwrap_or_else(|e| e.into_inner()));
        for (key, r) in done {
            self.nw.choosing.remove(&key);
            let Ok(path) = r else { continue };
            if path.is_empty() {
                continue;
            }
            if key == "export" {
                self.nw.export_path = tilde(&path);
            } else if key == "import" {
                self.nw.import_path = tilde(&path);
            } else if let Some(rest) = key.strip_prefix("field:") {
                // field:<instance id>:<path in settings>
                let Some((id, field)) = rest.split_once(':') else { continue };
                if let Some(w) = self.app.cfg.notch_shelf.widget_mut(id) {
                    set_at_path(obj_mut(w), field, json!(tilde(&path)));
                }
            }
        }
        if !self.nw.choosing.is_empty() || self.nw.tests.lock().unwrap_or_else(|e| e.into_inner()).values().any(|t| matches!(t, TestRun::Running(_))) {
            // Keep polling while something runs elsewhere.
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    fn pick(&mut self, key: String, what: &'static str, prompt: String) {
        if !self.nw.choosing.insert(key.clone()) {
            return;
        }
        let out = self.nw.picks.clone();
        std::thread::spawn(move || {
            let r = choose_path(what, prompt);
            out.lock().unwrap_or_else(|e| e.into_inner()).push((key, r));
        });
    }

    fn notch_running(&self) -> bool {
        self.notch_status.as_ref().is_some_and(|s| s.running)
    }

    // ----- pages -------------------------------------------------------------

    fn pages_card(&mut self, ui: &mut Ui) {
        let running = self.notch_running();
        let nw = &mut self.nw;
        let n = &mut self.app.cfg.notch_shelf;
        if n.pages.is_empty() {
            n.pages = WidgetPage::defaults();
        }
        let mut show_in_notch = None;
        card_with(
            ui,
            "Pages",
            Some("The open notch shows one page at a time; switch with the tabs at the top or a two-finger swipe."),
            |ui| {
                block(ui, |ui| {
                    let counts: Vec<usize> = (0..n.pages.len())
                        .map(|k| n.widgets.iter().filter(|w| w.page.min(n.pages.len() - 1) == k && w.enabled).count())
                        .collect();
                    let mut go = None;
                    let mut add = false;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        for (k, pg) in n.pages.iter().enumerate() {
                            if page_tab(ui, &pg.name, &pg.symbol, counts[k], k == nw.page).clicked() {
                                go = Some(k);
                            }
                        }
                        if ui.add(egui::Button::new(theme::icon_job(icon::PLUS, "Add page", 13.0, pal(ui).text)).min_size(vec2(28.0, 28.0)).corner_radius(14.0)).clicked() {
                            add = true;
                        }
                    });
                    if let Some(k) = go {
                        nw.page = k;
                        nw.confirm = None;
                    }
                    if add && n.pages.len() < 12 {
                        n.pages.push(WidgetPage::named(&format!("Page {}", n.pages.len() + 1)));
                        nw.page = n.pages.len() - 1;
                    }
                });
                let k = nw.page.min(n.pages.len() - 1);
                let count = n.pages.len();
                row(ui, "Name", None, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut n.pages[k].name).hint_text("Home").desired_width(220.0));
                });
                row(ui, "Icon", Some("Shown in the page tabs instead of the name."), |ui| symbol_picker(ui, &mut n.pages[k].symbol));
                let mut mv = None;
                let mut delete = false;
                row(ui, "Arrange", None, |ui| {
                    if ui.add_enabled(k > 0, egui::Button::new(theme::icon_job(icon::CARET_LEFT, "Move left", 13.0, pal(ui).text))).clicked() {
                        mv = Some(k - 1);
                    }
                    if ui.add_enabled(k + 1 < count, egui::Button::new(theme::icon_job(icon::CARET_RIGHT, "Move right", 13.0, pal(ui).text))).clicked() {
                        mv = Some(k + 1);
                    }
                    let key = format!("page:{k}");
                    if nw.confirm.as_deref() == Some(key.as_str()) {
                        if danger_button(ui, "Delete page and move its widgets").clicked() {
                            delete = true;
                        }
                        if ui.button("Cancel").clicked() {
                            nw.confirm = None;
                        }
                    } else if ui.add_enabled(count > 1, egui::Button::new(theme::icon_job(icon::TRASH, "Delete", 13.0, if count > 1 { pal(ui).error } else { pal(ui).text3 }))).on_disabled_hover_text("The last page can't be deleted").clicked() {
                        nw.confirm = Some(key);
                    }
                    if running && cfg!(target_os = "macos") && icon_button(ui, icon::ARROW_SQUARE_OUT, "Open the notch on this page").clicked() {
                        show_in_notch = Some(k);
                    }
                });
                if let Some(to) = mv {
                    if notch_shelf::widgets::instance::move_page(&mut n.pages, &mut n.widgets, k, to) {
                        nw.page = to;
                    }
                }
                if delete {
                    notch_shelf::widgets::instance::remove_page(&mut n.pages, &mut n.widgets, k);
                    nw.page = k.saturating_sub(1).min(n.pages.len() - 1);
                    nw.confirm = None;
                }
            },
        );
        if let Some(page) = show_in_notch {
            let _ = pm_config::bridge::write(&self.config_path, pm_config::bridge::NOTCH_REQUEST_FILE, &NotchRequest::ShowPage { page });
            self.toast = Some((Instant::now(), "Opened in the notch".into()));
        }
    }

    // ----- widgets on the page ------------------------------------------------

    fn widgets_card(&mut self, ui: &mut Ui) {
        let nw = &mut self.nw;
        let n = &mut self.app.cfg.notch_shelf;
        let pages = n.page_list();
        let k = nw.page.min(pages.len() - 1);
        let last = pages.len() - 1;
        let idx: Vec<usize> = (0..n.widgets.len()).filter(|&i| n.widgets[i].page.min(last) == k).collect();
        let title = format!("Widgets on \u{201c}{}\u{201d}", if pages[k].name.trim().is_empty() { "Untitled" } else { pages[k].name.trim() });
        let mut action: Option<RowAction> = None;
        let panel_w = n.panel_width as f64;
        card_with(ui, &title, Some("Drag the handle or use the menu to reorder. Click a widget to edit it."), |ui| {
            block(ui, |ui| {
                let list: Vec<&WidgetInstance> = idx.iter().map(|&i| &n.widgets[i]).collect();
                if let Some(clicked) = notch_preview(ui, &list, nw.selected.as_deref(), panel_w) {
                    action = Some(RowAction::Select(clicked));
                }
            });
            if idx.is_empty() {
                block(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(6.0);
                        ui.label(RichText::new("No widgets on this page yet").font(theme::font_semibold(13.5)).color(pal(ui).text));
                        hint(ui, "Add one from the gallery: a clock, a script, your Shortcuts, …");
                        ui.add_space(6.0);
                    });
                });
            }
            let drag_layer = ui.scope_id().with("nw-drag");
            let mut rects: Vec<Rect> = Vec::with_capacity(idx.len());
            let mut dragging: Option<usize> = None;
            for (pos, &i) in idx.iter().enumerate() {
                let w = &mut n.widgets[i];
                let wid = w.id.clone();
                ui.push_id(("nw-row", &wid), |ui| {
                    let (r, a, dragged) = instance_row(ui, w, pos, idx.len(), nw.selected.as_deref() == Some(w.id.as_str()), &pages, k, nw.confirm.as_deref() == Some(w.id.as_str()));
                    rects.push(r);
                    if dragged {
                        dragging = Some(pos);
                    }
                    if a.is_some() {
                        action = a;
                    }
                });
            }
            // Drag to reorder: an insertion line follows the pointer.
            if let Some(from) = dragging {
                if let Some(ptr) = ui.ctx().pointer_interact_pos() {
                    let to = rects.iter().position(|r| ptr.y < r.center().y).unwrap_or(rects.len());
                    let y = if to < rects.len() { rects[to].top() } else { rects.last().map(|r| r.bottom()).unwrap_or(ptr.y) };
                    let x = rects.first().map(|r| r.x_range()).unwrap_or(ui.max_rect().x_range());
                    let p = pal(ui);
                    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, drag_layer));
                    painter.hline(x, y, Stroke::new(2.5, p.accent));
                    painter.circle_filled(pos2(x.min, y), 4.0, p.accent);
                    if ui.input(|i| i.pointer.any_released()) {
                        let to = if to > from { to - 1 } else { to };
                        if to != from {
                            action = Some(RowAction::Move(from, to));
                        }
                    }
                }
            }
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    let label = if nw.gallery { "Close gallery" } else { "Add widget" };
                    let glyph = if nw.gallery { icon::X } else { icon::PLUS };
                    if (if nw.gallery { icon_text_button(ui, glyph, label) } else { primary_icon_button(ui, glyph, label) }).clicked() {
                        nw.gallery = !nw.gallery;
                    }
                    let on = idx.iter().filter(|&&i| n.widgets[i].enabled).count();
                    ui.label(RichText::new(format!("{} shown · {} in all", on, plural(idx.len(), "widget"))).size(12.0).color(pal(ui).text2));
                });
            });
        });
        match action {
            Some(RowAction::Select(id)) => {
                nw.selected = if nw.selected.as_deref() == Some(id.as_str()) { None } else { Some(id) };
                nw.confirm = None;
            }
            Some(RowAction::Move(from, to)) => {
                if from < idx.len() && to < idx.len() {
                    let w = n.widgets.remove(idx[from]);
                    n.widgets.insert(idx[to], w);
                }
            }
            Some(RowAction::ToPage(id, page)) => {
                if let Some(w) = n.widget_mut(&id) {
                    w.page = page;
                }
            }
            Some(RowAction::Duplicate(id)) => {
                if let Some(pos) = n.widgets.iter().position(|w| w.id == id) {
                    let mut copy = n.widgets[pos].clone();
                    copy.id = notch_shelf::new_id();
                    let new_id = copy.id.clone();
                    n.widgets.insert(pos + 1, copy);
                    nw.selected = Some(new_id);
                }
            }
            Some(RowAction::AskDelete(id)) => nw.confirm = Some(id),
            Some(RowAction::CancelDelete) => nw.confirm = None,
            Some(RowAction::Delete(id)) => {
                n.remove_widget(&id);
                nw.tests.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                if nw.selected.as_deref() == Some(id.as_str()) {
                    nw.selected = None;
                }
                nw.confirm = None;
            }
            None => {}
        }
    }

    // ----- gallery ---------------------------------------------------------------

    fn gallery_card(&mut self, ui: &mut Ui) {
        let nw = &mut self.nw;
        let n = &mut self.app.cfg.notch_shelf;
        let page = nw.page;
        let mut add: Option<WidgetKind> = None;
        let catalog = notch_shelf::widget_catalog();
        card_with(ui, "Widget gallery", Some("Click a widget to add it to this page. You can add the same kind several times with different settings."), |ui| {
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    let p = pal(ui);
                    ui.label(theme::icon_text(icon::MAGNIFYING_GLASS).size(14.0).color(p.text2));
                    ui.add(egui::TextEdit::singleline(&mut nw.gallery_query).hint_text("Search widgets").desired_width(260.0));
                });
            });
            let q = nw.gallery_query.trim().to_lowercase();
            for cat in CATEGORIES {
                let kinds: Vec<&WidgetKindInfo> = catalog
                    .iter()
                    .filter(|i| i.category == cat)
                    .filter(|i| q.is_empty() || i.title.to_lowercase().contains(&q) || i.description.to_lowercase().contains(&q) || category_name(cat).to_lowercase().contains(&q))
                    .collect();
                if kinds.is_empty() {
                    continue;
                }
                block(ui, |ui| {
                    let p = pal(ui);
                    ui.label(RichText::new(category_name(cat)).font(theme::font_semibold(13.5)).color(p.text));
                    ui.label(RichText::new(category_blurb(cat)).size(12.0).color(p.text2));
                    ui.add_space(6.0);
                    let cols = if ui.available_width() > 560.0 { 3 } else { 2 };
                    let gap = 10.0;
                    let tw = ((ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32).floor();
                    for chunk in kinds.chunks(cols) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            for info in chunk {
                                let count = n.widgets.iter().filter(|w| w.kind == info.kind).count();
                                if gallery_tile(ui, info, tw, count).clicked() {
                                    add = Some(info.kind);
                                }
                            }
                        });
                        ui.add_space(gap - 8.0);
                    }
                });
            }
        });
        if let Some(kind) = add {
            let w = WidgetInstance::new(kind).on_page(page);
            let id = n.add_widget(w);
            nw.selected = Some(id);
            nw.gallery = false;
            nw.scroll_to_editor = true;
            self.toast = Some((Instant::now(), format!("Added {}", notch_shelf::kind_info(kind).title)));
        }
    }

    // ----- editor -------------------------------------------------------------------

    fn editor_card(&mut self, ui: &mut Ui) {
        let Some(id) = self.nw.selected.clone() else { return };
        // Lazy helpers (never while the search indexes the page).
        let kind = match self.app.cfg.notch_shelf.widgets.iter().find(|w| w.id == id) {
            Some(w) => w.kind,
            None => return,
        };
        if ui.is_visible() {
            if kind == WidgetKind::Shortcuts && !self.nw.shortcuts_requested {
                self.nw.shortcuts_requested = true;
                let out = self.nw.shortcuts.clone();
                std::thread::spawn(move || {
                    let list = notch_shelf::list_shortcuts();
                    *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(list);
                });
            }
            if kind == WidgetKind::Launcher && self.nw.apps.is_none() {
                self.nw.apps = Some(installed_apps());
            }
        }
        let pages = self.app.cfg.notch_shelf.page_list();
        let status = self.notch_status.clone();
        let shortcuts = self.nw.shortcuts.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let test = self.nw.tests.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
        let apps = self.nw.apps.clone().unwrap_or_default();
        let mut pick: Option<(String, &'static str, String)> = None;
        let mut run_test = false;
        let mut refresh_in_app = false;
        let mut open_folder: Option<std::path::PathBuf> = None;
        let mut close = false;
        let scroll = std::mem::take(&mut self.nw.scroll_to_editor);
        let nw = &mut self.nw;
        // xbar folder listing (cached a few seconds).
        let xbar_path = if kind == WidgetKind::Xbar {
            let w = self.app.cfg.notch_shelf.widgets.iter().find(|w| w.id == id).unwrap();
            let p = text_of(w.settings.get("path"));
            Some(if p.trim().is_empty() { notch_shelf::widgets::catalog::DEFAULT_PLUGIN_DIR.to_string() } else { p })
        } else {
            None
        };
        if let Some(p) = &xbar_path {
            let stale = nw.xbar_cache.as_ref().is_none_or(|(q, t, _)| q != p || t.elapsed() > Duration::from_secs(3));
            if stale && ui.is_visible() {
                nw.xbar_cache = Some((p.clone(), Instant::now(), xbar_plugins(p)));
            }
        }
        let xbar_list = nw.xbar_cache.as_ref().map(|c| c.2.clone()).unwrap_or_default();
        let Some(w) = self.app.cfg.notch_shelf.widget_mut(&id) else { return };
        let info = notch_shelf::kind_info(kind);
        let errors = match notch_shelf::validate_settings(kind, &w.settings) {
            Ok(_) => vec![],
            Err(e) => e,
        };
        let title = format!("{} settings", instance_title(w));
        let r = card(ui, &title, |ui| {
            // Header: icon, kind, description, permission note.
            block(ui, |ui| {
                let p = pal(ui);
                ui.horizontal(|ui| {
                    icon_tile(ui, sf_glyph(info.sf_symbol), kind_tint(kind), 40.0);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(info.title).font(theme::font_semibold(15.0)).color(p.text));
                            if !errors.is_empty() {
                                tag_pill(ui, p.warn, icon::WARNING, &format!("{} to fix", plural(errors.len(), "setting")));
                            }
                        });
                        ui.add(egui::Label::new(RichText::new(info.description).size(12.5).color(p.text2)).wrap());
                    });
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if icon_button(ui, icon::X, "Close").clicked() {
                            close = true;
                        }
                    });
                });
                if let Some(note) = info.permission_note {
                    ui.add_space(6.0);
                    note_line(ui, icon::SHIELD_CHECK, note);
                }
            });
            toggle_row(ui, "Show this widget", Some("Off keeps it with its settings but hides it."), &mut w.enabled);
            row(ui, "Size", Some("Small is one unit wide, medium two, large three."), |ui| {
                let mut s = w.effective_size();
                let items: Vec<(WidgetSize, &str)> = info.sizes.iter().map(|s| (*s, s.label())).collect();
                if segmented(ui, &mut s, &items).changed() {
                    w.size = s;
                }
            });
            row(ui, "Page", None, |ui| {
                let last = pages.len() - 1;
                let cur = w.page.min(last);
                egui::ComboBox::from_id_salt(("nw-page", &id)).selected_text(page_label(&pages[cur], cur)).width(200.0).show_ui(ui, |ui| {
                    for (k, pg) in pages.iter().enumerate() {
                        if ui.selectable_label(k == cur, page_label(pg, k)).clicked() {
                            w.page = k;
                        }
                    }
                });
            });
            let mut ctx = FormCtx {
                kind,
                id: id.clone(),
                secrets_shown: &mut nw.secrets_shown,
                shortcuts: shortcuts.as_deref(),
                reminder_lists: status.as_ref().map(|s| s.reminder_lists.clone()).unwrap_or_default(),
                apps: &apps,
                pick: &mut pick,
                choosing: &nw.choosing,
            };
            let fields = info.fields.clone();
            if !fields.is_empty() {
                let obj = obj_mut(w);
                settings_form(ui, &mut ctx, &fields, obj, "", &errors);
            }
            // Kind helpers.
            match kind {
                WidgetKind::Xbar => {
                    block(ui, |ui| {
                        let p = pal(ui);
                        let folder = xbar_path.clone().unwrap_or_default();
                        ui.label(RichText::new("Plugin folder").font(theme::font_semibold(13.0)).color(p.text));
                        hint(ui, "Put xbar / SwiftBar plugins (name.5m.sh, name.1h.py, …) here and make them executable. The number in the name is how often each runs.");
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let pw = (ui.available_width() - 200.0).max(120.0);
                            egui::Frame::new().fill(p.control).corner_radius(7.0).inner_margin(egui::Margin::symmetric(9, 5)).show(ui, |ui| {
                                ui.set_max_width(pw);
                                ui.add(egui::Label::new(RichText::new(tilde(&expand(&folder).to_string_lossy())).monospace().size(12.0).color(p.text)).truncate()).on_hover_text(&folder);
                            });
                            if icon_text_button(ui, icon::FOLDER_OPEN, "Open plugins folder").clicked() {
                                open_folder = Some(expand(&folder));
                            }
                        });
                        ui.add_space(4.0);
                        if xbar_list.is_empty() {
                            ui.label(RichText::new("No plugins there yet. Get some at xbarapp.com, or write one: anything that prints a line is a plugin.").size(12.0).color(p.text3));
                        } else {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for (name, every) in xbar_list.iter().take(8) {
                                ui.horizontal(|ui| {
                                    ui.label(theme::icon_text(glyph::PUZZLE).size(13.0).color(p.text2));
                                    ui.label(RichText::new(name).monospace().size(12.0).color(p.text));
                                    ui.label(RichText::new(every).size(11.5).color(p.text3));
                                });
                            }
                            if xbar_list.len() > 8 {
                                ui.label(RichText::new(format!("and {} more", xbar_list.len() - 8)).size(11.5).color(p.text3));
                            }
                        }
                    });
                }
                WidgetKind::Script => {
                    block(ui, |ui| {
                        ui.collapsing(RichText::new("Output formats").size(12.5), script_format_help);
                    });
                }
                WidgetKind::Web => {
                    block(ui, |ui| {
                        ui.collapsing(RichText::new("Template syntax").size(12.5), web_template_help);
                    });
                }
                _ => {}
            }
            if matches!(kind, WidgetKind::Script | WidgetKind::Xbar | WidgetKind::Web) {
                block(ui, |ui| {
                    let p = pal(ui);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Preview").font(theme::font_semibold(13.0)).color(p.text));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let running = matches!(test, Some(TestRun::Running(_)));
                            if ui.add_enabled(!running && errors.is_empty(), egui::Button::new(theme::icon_job(icon::PLAY, if running { "Running…" } else { "Test" }, 13.0, p.text))).on_disabled_hover_text("Fix the settings first").clicked() {
                                run_test = true;
                            }
                            if status.as_ref().is_some_and(|s| s.running) && icon_button(ui, icon::ARROWS_CLOCKWISE, "Refresh it in the notch now").clicked() {
                                refresh_in_app = true;
                            }
                        });
                    });
                    let (preview, caption) = match (&test, status.as_ref().and_then(|s| s.widgets.get(&id))) {
                        (Some(TestRun::Done { result, took, at }), _) => (Some((**result).clone()), format!("Test run · took {:.1} s · {}", took.as_secs_f32(), ago(*at))),
                        (Some(TestRun::Running(t)), _) => (None, format!("Running… {:.0} s", t.elapsed().as_secs_f32())),
                        (None, Some(pv)) => (Some(pv.clone()), "Latest result in the notch".into()),
                        (None, None) => (None, "Click Test to run it once and see what the card will show. Nothing runs until you do.".into()),
                    };
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        notch_card_preview(ui, w.effective_size(), kind, &instance_title(w), &text_of(w.settings.get("symbol")), preview.as_ref());
                    });
                    ui.label(RichText::new(caption).size(11.5).color(p.text3));
                });
            }
        });
        if scroll {
            ui.scroll_to_rect(r.rect, Some(Align::TOP));
        }
        let snapshot = w.clone();
        if close {
            self.nw.selected = None;
        }
        if let Some((key, what, prompt)) = pick {
            self.pick(key, what, prompt);
        }
        if let Some(dir) = open_folder {
            let _ = std::fs::create_dir_all(&dir);
            crate::platform::open_folder(&dir);
        }
        if refresh_in_app {
            let _ = pm_config::bridge::write(&self.config_path, pm_config::bridge::NOTCH_REQUEST_FILE, &NotchRequest::RefreshWidget { id: id.clone() });
            self.toast = Some((Instant::now(), "Refreshing in the notch".into()));
        }
        if run_test {
            self.start_test(snapshot);
        }
    }

    fn start_test(&mut self, inst: WidgetInstance) {
        let tests = self.nw.tests.clone();
        tests.lock().unwrap_or_else(|e| e.into_inner()).insert(inst.id.clone(), TestRun::Running(Instant::now()));
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let r = notch_shelf::run_widget_once(&inst);
            let done = TestRun::Done { result: Box::new(WidgetPreview::from_result(&r)), took: t0.elapsed(), at: Instant::now() };
            tests.lock().unwrap_or_else(|e| e.into_inner()).insert(inst.id.clone(), done);
        });
    }

    // ----- global options -------------------------------------------------------------

    fn home_options_card(&mut self, ui: &mut Ui) {
        use notch_shelf::DefaultView;
        let n = &mut self.app.cfg.notch_shelf;
        card(ui, "Home view", |ui| {
            row(ui, "Open in", Some("A drag always opens the shelf."), |ui| {
                segmented(ui, &mut n.default_view, &[(DefaultView::Last, "Last view"), (DefaultView::Home, "Widgets"), (DefaultView::Shelf, "Shelf")])
            });
            toggle_row(
                ui,
                "Live activities",
                Some("Music, a running timer and widgets with “Live activity” on show beside the closed notch."),
                &mut n.live_activities,
            );
        });
    }

    // ----- export / import -------------------------------------------------------------

    fn share_card(&mut self, ui: &mut Ui) {
        let mac = cfg!(target_os = "macos");
        let mut export = false;
        let mut import = false;
        let mut undo = false;
        let mut pick: Option<(&'static str, &'static str)> = None;
        let mut reveal: Option<std::path::PathBuf> = None;
        let nw = &mut self.nw;
        card_with(ui, "Share layout", Some("Save the pages and widgets to a file, or load a layout someone shared. API tokens stay out of exports unless you include them."), |ui| {
            row(ui, "Export to", None, |ui| {
                ui.add(egui::TextEdit::singleline(&mut nw.export_path).hint_text("~/Downloads/notch-layout.json").desired_width(250.0));
                if mac && ui.add_enabled(!nw.choosing.contains("export"), egui::Button::new("Choose…")).clicked() {
                    pick = Some(("export", "save"));
                }
            });
            toggle_row(ui, "Include API tokens", Some("Only for your own backups: anyone with the file can use them."), &mut nw.export_secrets);
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!nw.export_path.trim().is_empty(), egui::Button::new(theme::icon_job(glyph::EXPORT, "Export layout", 13.0, pal(ui).text))).clicked() {
                        export = true;
                    }
                });
            });
            row(ui, "Import from", None, |ui| {
                ui.add(egui::TextEdit::singleline(&mut nw.import_path).hint_text("notch-layout.json").desired_width(250.0));
                if mac && ui.add_enabled(!nw.choosing.contains("import"), egui::Button::new("Choose…")).clicked() {
                    pick = Some(("import", "file"));
                }
            });
            row(ui, "Imported widgets", None, |ui| {
                segmented(ui, &mut nw.import_append, &[(false, "Replace this layout"), (true, "Add as new pages")])
            });
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!nw.import_path.trim().is_empty(), egui::Button::new(theme::icon_job(glyph::DOWNLOAD, "Import layout", 13.0, pal(ui).text))).clicked() {
                        import = true;
                    }
                    if nw.undo.is_some() && icon_text_button(ui, icon::ARROW_COUNTER_CLOCKWISE, "Undo import").clicked() {
                        undo = true;
                    }
                });
                if let Some((ok, msg, warnings)) = &nw.io_result {
                    ui.add_space(6.0);
                    if *ok {
                        ok_label(ui, msg);
                    } else {
                        error_label(ui, msg);
                    }
                    for w in warnings {
                        warn_label(ui, w);
                    }
                    if *ok && msg.starts_with("Exported") && mac && ui.link("Show in Finder").clicked() {
                        reveal = Some(expand(&nw.export_path));
                    }
                }
            });
        });
        if let Some((key, what)) = pick {
            let prompt = if key == "export" { "Export the notch layout" } else { "Choose a notch layout (.json)" };
            self.pick(key.into(), what, prompt.into());
        }
        if let Some(p) = reveal {
            crate::platform::reveal_path(&p);
        }
        let n = &mut self.app.cfg.notch_shelf;
        let nw = &mut self.nw;
        if export {
            let path = expand(&nw.export_path);
            let text = n.export_layout(nw.export_secrets);
            nw.io_result = Some(match std::fs::write(&path, text) {
                Ok(()) => (true, format!("Exported {} on {} to {}", plural(n.widgets.len(), "widget"), plural(n.page_list().len(), "page"), tilde(&path.to_string_lossy())), vec![]),
                Err(e) => (false, format!("Couldn't write {}: {e}", tilde(&path.to_string_lossy())), vec![]),
            });
        }
        if import {
            let path = expand(&nw.import_path);
            nw.io_result = Some(match std::fs::read_to_string(&path) {
                Err(e) => (false, format!("Couldn't read {}: {e}", tilde(&path.to_string_lossy())), vec![]),
                Ok(text) => {
                    let before = (n.page_list(), n.widgets.clone());
                    let mode = if nw.import_append { ImportMode::Append } else { ImportMode::Replace };
                    match n.import_layout(&text, mode) {
                        Ok(warnings) => {
                            nw.undo = Some(before);
                            nw.selected = None;
                            nw.page = if mode == ImportMode::Append { nw.page } else { 0 };
                            (true, format!("Imported: now {} on {}", plural(n.widgets.len(), "widget"), plural(n.page_list().len(), "page")), warnings)
                        }
                        Err(e) => (false, e, vec![]),
                    }
                }
            });
        }
        if undo {
            if let Some((pages, widgets)) = nw.undo.take() {
                n.pages = pages;
                n.widgets = widgets;
                nw.page = 0;
                nw.selected = None;
                nw.io_result = Some((true, "Import undone".into(), vec![]));
            }
        }
    }
}

fn ago(t: Instant) -> String {
    let s = t.elapsed().as_secs();
    if s < 5 {
        "just now".into()
    } else if s < 60 {
        format!("{s} s ago")
    } else {
        format!("{} min ago", s / 60)
    }
}

fn page_label(p: &WidgetPage, k: usize) -> String {
    if p.name.trim().is_empty() {
        format!("Page {}", k + 1)
    } else {
        p.name.trim().to_string()
    }
}

/// A tinted notice with a switch (like the hero's).
fn notice_with_switch(ui: &mut Ui, title: &str, text: &str, on: &mut bool) {
    let p = pal(ui);
    let c = Color32::from_rgb(0xE8, 0x9A, 0x00);
    ui.add_space(16.0);
    egui::Frame::new()
        .fill(theme::mix(p.card, c, if p.dark { 0.16 } else { 0.08 }))
        .stroke(Stroke::new(1.0, theme::mix(p.card_border, c, 0.4)))
        .corner_radius(12.0)
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::icon_text(icon::WARNING).size(18.0).color(c));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(title).font(theme::font_semibold(13.5)).color(p.text));
                    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.text2)).wrap());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, "Turn on").clicked() {
                        *on = true;
                    }
                });
            });
        });
}

/// A small line with an icon (permission notes).
fn note_line(ui: &mut Ui, g: &str, text: &str) {
    let p = pal(ui);
    egui::Frame::new().fill(p.control).corner_radius(8.0).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(theme::icon_text(g).size(14.0).color(p.text2));
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(p.text2)).wrap());
        });
    });
}

/// A red-tinted confirmation button.
fn danger_button(ui: &mut Ui, text: &str) -> egui::Response {
    let p = pal(ui);
    ui.add(egui::Button::new(RichText::new(text).font(theme::font_semibold(12.5)).color(Color32::WHITE)).fill(p.error).stroke(Stroke::NONE))
}

/// A page tab: icon or name, widget count; selected ones are accented.
fn page_tab(ui: &mut Ui, name: &str, symbol: &str, count: usize, selected: bool) -> egui::Response {
    let p = pal(ui);
    let name = if name.trim().is_empty() { "Untitled" } else { name.trim() };
    let font = if selected { theme::font_semibold(13.0) } else { theme::font(13.0) };
    let fg = if selected { p.on_accent } else { p.text };
    let tg = ui.fonts_mut(|f| f.layout_no_wrap(name.to_string(), font.clone(), fg));
    let count_text = count.to_string();
    let cg = ui.fonts_mut(|f| f.layout_no_wrap(count_text.clone(), theme::font_semibold(11.0), fg));
    let has_icon = !symbol.is_empty();
    let w = 14.0 + if has_icon { 20.0 } else { 0.0 } + tg.size().x + 8.0 + (cg.size().x + 12.0).max(20.0) + 8.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
    let hovered = resp.hovered();
    let fill = if selected {
        p.accent
    } else if hovered {
        p.control_hover
    } else {
        p.control
    };
    let painter = ui.painter();
    painter.rect(rect, 15.0, fill, Stroke::new(1.0, if selected { p.accent } else { p.control_border.gamma_multiply(0.6) }), StrokeKind::Inside);
    let mut x = rect.left() + 14.0;
    if has_icon {
        painter.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, sf_glyph(symbol), theme::icon_font(14.0), fg);
        x += 20.0;
    }
    painter.galley(pos2(x, rect.center().y - tg.size().y / 2.0), tg.clone(), fg);
    x += tg.size().x + 8.0;
    let badge = Rect::from_min_size(pos2(x, rect.center().y - 9.0), vec2((cg.size().x + 12.0).max(20.0), 18.0));
    painter.rect_filled(badge, 9.0, if selected { Color32::from_white_alpha(56) } else { p.card });
    painter.galley(badge.center() - cg.size() / 2.0, cg, if selected { p.on_accent } else { p.text2 });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The page icon chooser: a button showing the icon, opening a grid.
fn symbol_picker(ui: &mut Ui, symbol: &mut String) {
    let p = pal(ui);
    let current = PAGE_SYMBOLS.iter().find(|(s, _)| s == symbol).map(|(_, l)| *l).unwrap_or(if symbol.is_empty() { "None" } else { "Custom" });
    let label = if symbol.is_empty() { egui::WidgetText::from(RichText::new("None").color(p.text)) } else { egui::WidgetText::from(theme::icon_job(sf_glyph(symbol), current, 13.0, p.text)) };
    ui.menu_button(label, |ui| {
        ui.set_min_width(268.0);
        ui.label(RichText::new("Page icon").font(theme::font_semibold(12.5)).color(p.text));
        ui.add_space(4.0);
        egui::Grid::new("nw-symbols").spacing([4.0, 4.0]).show(ui, |ui| {
            for (i, (s, l)) in PAGE_SYMBOLS.iter().enumerate() {
                let sel = symbol == s;
                let text = if s.is_empty() { RichText::new("Aa").size(13.0) } else { theme::icon_text(sf_glyph(s)).size(17.0) };
                let b = egui::Button::new(text.color(if sel { p.on_accent } else { p.text })).min_size(vec2(36.0, 32.0)).fill(if sel { p.accent } else { p.control });
                if ui.add(b).on_hover_text(*l).clicked() {
                    *symbol = s.to_string();
                    ui.close();
                }
                if i % 7 == 6 {
                    ui.end_row();
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("SF Symbol").size(12.0).color(p.text2));
            ui.add(egui::TextEdit::singleline(symbol).hint_text("house.fill").desired_width(160.0));
        });
    });
}

enum RowAction {
    Select(String),
    Move(usize, usize),
    ToPage(String, usize),
    Duplicate(String),
    AskDelete(String),
    CancelDelete,
    Delete(String),
}

/// One widget in the page's list. Returns its rect, what was clicked and
/// whether its handle is being dragged.
#[allow(clippy::too_many_arguments)]
fn instance_row(ui: &mut Ui, w: &mut WidgetInstance, pos: usize, len: usize, selected: bool, pages: &[WidgetPage], page: usize, confirming: bool) -> (Rect, Option<RowAction>, bool) {
    let p = pal(ui);
    let mut action = None;
    // Separator like the card rows.
    divider(ui);
    let top = ui.cursor().top();
    let full = ui.available_width();
    let h = 56.0;
    let rect = Rect::from_min_size(pos2(ui.max_rect().left(), top), vec2(full, h));
    let bg = ui.interact(rect, ui.scope_id().with("bg"), Sense::click());
    if selected {
        ui.painter().rect_filled(rect.expand2(vec2(10.0, 0.0)), 8.0, p.accent_soft);
    } else if bg.hovered() {
        ui.painter().rect_filled(rect.expand2(vec2(10.0, 0.0)), 8.0, p.control.gamma_multiply(0.6));
    }
    if bg.clicked() {
        action = Some(RowAction::Select(w.id.clone()));
    }
    let mut dragged = false;
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
    {
        let ui = &mut child;
        // Drag handle.
        let (hr, hresp) = ui.allocate_exact_size(vec2(18.0, h), Sense::drag());
        let hresp = hresp.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text("Drag to reorder");
        if hresp.dragged() {
            dragged = true;
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        ui.painter().text(hr.center(), Align2::CENTER_CENTER, icon::DOTS_SIX_VERTICAL, theme::icon_font(16.0), if hresp.hovered() || dragged { p.text2 } else { p.text3 });
        ui.add_space(4.0);
        let info = notch_shelf::kind_info(w.kind);
        let tint = if w.enabled { kind_tint(w.kind) } else { theme::mix(kind_tint(w.kind), p.off, 0.7) };
        icon_tile(ui, sf_glyph(info.sf_symbol), tint, 32.0);
        ui.add_space(6.0);
        // Text (leaves room for the controls on the right).
        let text_w = (ui.available_width() - 250.0).max(120.0);
        ui.allocate_ui_with_layout(vec2(text_w, h), Layout::top_down(Align::Min), |ui| {
            ui.add_space(10.0);
            ui.spacing_mut().item_spacing.y = 1.0;
            let tc = if w.enabled { p.text } else { p.text3 };
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add(egui::Label::new(RichText::new(instance_title(w)).font(theme::font_semibold(13.5)).color(tc)).truncate());
                if notch_shelf::validate_settings(w.kind, &w.settings).is_err() {
                    ui.label(theme::icon_text(icon::WARNING).size(13.0).color(p.warn)).on_hover_text("Some settings need attention");
                }
            });
            let sub = if instance_title(w) == info.title || w.kind == WidgetKind::Unknown { instance_summary(w) } else { format!("{} · {}", info.title, instance_summary(w)) };
            ui.add(egui::Label::new(RichText::new(sub).size(12.0).color(p.text2)).truncate());
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if confirming {
                if ui.button("Cancel").clicked() {
                    action = Some(RowAction::CancelDelete);
                }
                if danger_button(ui, "Delete").clicked() {
                    action = Some(RowAction::Delete(w.id.clone()));
                }
                return;
            }
            // More: move, duplicate, delete.
            ui.menu_button(theme::icon_text(glyph::DOTS_THREE).size(16.0).color(p.text), |ui| {
                ui.set_min_width(190.0);
                if ui.add_enabled(pos > 0, egui::Button::new(theme::icon_job(icon::ARROW_UP, "Move up", 13.0, p.text))).clicked() {
                    action = Some(RowAction::Move(pos, pos - 1));
                    ui.close();
                }
                if ui.add_enabled(pos + 1 < len, egui::Button::new(theme::icon_job(icon::ARROW_DOWN, "Move down", 13.0, p.text))).clicked() {
                    action = Some(RowAction::Move(pos, pos + 1));
                    ui.close();
                }
                if pages.len() > 1 {
                    ui.menu_button(theme::icon_job(icon::ARROWS_LEFT_RIGHT, "Move to page", 13.0, p.text), |ui| {
                        for (k, pg) in pages.iter().enumerate() {
                            if ui.add_enabled(k != page, egui::Button::new(page_label(pg, k))).clicked() {
                                action = Some(RowAction::ToPage(w.id.clone(), k));
                                ui.close();
                            }
                        }
                    });
                }
                if ui.button(theme::icon_job(icon::COPY, "Duplicate", 13.0, p.text)).clicked() {
                    action = Some(RowAction::Duplicate(w.id.clone()));
                    ui.close();
                }
                ui.separator();
                if ui.button(theme::icon_job(icon::TRASH, "Delete…", 13.0, p.error)).clicked() {
                    action = Some(RowAction::AskDelete(w.id.clone()));
                    ui.close();
                }
            })
            .response
            .on_hover_text("More");
            let name = format!("Show {}", instance_title(w));
            toggle_labeled(ui, &mut w.enabled, &name);
            // Size: only the sizes the kind allows.
            let allowed = notch_shelf::kind_info(w.kind).sizes.clone();
            if allowed.len() > 1 {
                let mut s = w.effective_size();
                let items: Vec<(WidgetSize, &str)> = allowed.iter().map(|s| (*s, size_letter(*s))).collect();
                if segmented(ui, &mut s, &items).on_hover_text("Size: small, medium, large").changed() {
                    w.size = s;
                }
            } else {
                tag_pill(ui, p.text2, glyph::SQUARE, w.effective_size().label());
            }
        });
    }
    ui.allocate_rect(rect, Sense::hover());
    (rect, action, dragged)
}

/// A to-scale picture of the page in the open notch; returns the id of a
/// card that was clicked.
fn notch_preview(ui: &mut Ui, list: &[&WidgetInstance], selected: Option<&str>, panel_w: f64) -> Option<String> {
    use notch_shelf::widgets::layout::{CARD_GAP, CARD_H, HOME_MAX_W};
    let p = pal(ui);
    let shown: Vec<&&WidgetInstance> = list.iter().filter(|w| w.enabled && w.kind != WidgetKind::Unknown).collect();
    let natural: f64 = shown.iter().map(|w| w.effective_size().width()).sum::<f64>() + CARD_GAP * (shown.len().saturating_sub(1)) as f64;
    let pad = 16.0;
    let home_w = (natural + 2.0 * pad).clamp(panel_w.clamp(380.0, 1400.0), HOME_MAX_W.max(panel_w));
    let avail = ui.available_width() as f64;
    let scale = (avail / (home_w + 40.0)).min(0.82);
    let panel = vec2((home_w * scale) as f32, ((CARD_H + 2.0 * pad + 26.0) * scale) as f32);
    let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), panel.y + 22.0), Sense::hover());
    let painter = ui.painter().clone();
    // The notch panel hangs from the top edge like the real one.
    let top = outer.top() + 4.0;
    let r = Rect::from_min_size(pos2(outer.center().x - panel.x / 2.0, top), panel);
    painter.rect_filled(Rect::from_min_max(pos2(outer.left(), top - 4.0), pos2(outer.right(), top)), 0.0, if p.dark { Color32::from_gray(44) } else { Color32::from_gray(214) });
    painter.rect_filled(r, egui::CornerRadius { nw: 0, ne: 0, sw: (22.0 * scale) as u8 + 6, se: (22.0 * scale) as u8 + 6 }, Color32::BLACK);
    // Header: tabs dots.
    let dots_y = r.top() + (13.0 * scale) as f32;
    painter.circle_filled(pos2(r.center().x - 5.0, dots_y), 2.2, Color32::from_gray(200));
    painter.circle_filled(pos2(r.center().x + 5.0, dots_y), 2.2, Color32::from_gray(90));
    let mut clicked = None;
    if shown.is_empty() {
        painter.text(r.center() + vec2(0.0, 6.0), Align2::CENTER_CENTER, "Only the shelf: no widgets on this page", theme::font(12.0), Color32::from_gray(150));
    } else {
        let row_w = natural * scale;
        let max_w = (home_w - 2.0 * pad) * scale;
        let mut x = r.left() as f64 + pad * scale + ((max_w - row_w).max(0.0)) / 2.0;
        let y = r.top() as f64 + (26.0 + pad) * scale;
        let clip = Rect::from_min_max(pos2(r.left() + (pad * scale) as f32 - 1.0, r.top()), pos2(r.right() - (pad * scale) as f32 + 1.0, r.bottom()));
        let cp = painter.with_clip_rect(clip);
        for w in &shown {
            let cw = w.effective_size().width() * scale;
            let cr = Rect::from_min_size(pos2(x as f32, y as f32), vec2(cw as f32, (CARD_H * scale) as f32));
            let id = ui.scope_id().with(("nw-prev", &w.id));
            let resp = ui.interact(cr.intersect(clip), id, Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            let sel = selected == Some(w.id.as_str());
            let fill = if resp.hovered() { Color32::from_gray(48) } else { Color32::from_gray(30) };
            cp.rect_filled(cr, (18.0 * scale) as f32, fill);
            if sel {
                cp.rect_stroke(cr, (18.0 * scale) as f32, Stroke::new(2.0, p.accent), StrokeKind::Outside);
            }
            let info = notch_shelf::kind_info(w.kind);
            let gs = (20.0 * scale).max(13.0) as f32;
            cp.text(cr.left_top() + vec2(10.0 * scale as f32 + 2.0, 10.0 * scale as f32 + 2.0), Align2::LEFT_TOP, sf_glyph(info.sf_symbol), theme::icon_font(gs), kind_tint(w.kind).linear_multiply(1.0));
            let title = instance_title(w);
            let tf = theme::font_semibold((11.5 * scale as f32).max(9.5));
            let tgal = ui.fonts_mut(|f| f.layout(title, tf, Color32::from_gray(225), cr.width() - 14.0));
            cp.galley(pos2(cr.left() + 8.0, cr.bottom() - tgal.size().y - 8.0), tgal, Color32::from_gray(225));
            if resp.clicked() {
                clicked = Some(w.id.clone());
            }
            x += cw + CARD_GAP * scale;
        }
        if row_w > max_w + 1.0 {
            // Scrolls in the notch: fade the right edge.
            let fade = Rect::from_min_max(pos2(clip.right() - 28.0, y as f32), pos2(clip.right(), (y + CARD_H * scale) as f32));
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(fade.left_top(), Color32::TRANSPARENT);
            mesh.colored_vertex(fade.right_top(), Color32::BLACK);
            mesh.colored_vertex(fade.right_bottom(), Color32::BLACK);
            mesh.colored_vertex(fade.left_bottom(), Color32::TRANSPARENT);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(mesh);
        }
    }
    let caption = if shown.is_empty() {
        String::new()
    } else {
        let units: usize = shown.iter().map(|w| w.effective_size().units()).sum();
        format!("{} · {} units wide{}", plural(shown.len(), "card"), units, if natural + 2.0 * pad > HOME_MAX_W.max(panel_w) { " · scrolls sideways" } else { "" })
    };
    painter.text(pos2(outer.center().x, outer.bottom() - 6.0), Align2::CENTER_BOTTOM, caption, theme::font(11.5), p.text3);
    clicked
}

/// A gallery tile: icon, title, description, sizes, permission note.
fn gallery_tile(ui: &mut Ui, info: &WidgetKindInfo, w: f32, count: usize) -> egui::Response {
    let p = pal(ui);
    let h = 154.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hovered = resp.hovered();
    let tint = kind_tint(info.kind);
    let painter = ui.painter().clone();
    painter.rect(
        rect,
        12.0,
        if hovered { theme::mix(p.card, tint, if p.dark { 0.10 } else { 0.05 }) } else { p.card },
        Stroke::new(if hovered { 1.5 } else { 1.0 }, if hovered { theme::mix(p.card_border, tint, 0.6) } else { p.card_border }),
        StrokeKind::Inside,
    );
    let ir = Rect::from_min_size(rect.left_top() + vec2(12.0, 12.0), vec2(34.0, 34.0));
    paint_icon_tile(&painter, ir, sf_glyph(info.sf_symbol), tint);
    let tx = ir.right() + 10.0;
    painter.text(pos2(tx, ir.top() + 1.0), Align2::LEFT_TOP, info.title, theme::font_semibold(13.5), p.text);
    // Sizes as S M L chips.
    let mut cx = tx;
    for s in WidgetSize::ALL {
        let ok = info.sizes.contains(&s);
        let cr = Rect::from_min_size(pos2(cx, ir.top() + 20.0), vec2(16.0, 14.0));
        if ok {
            painter.rect_filled(cr, 4.0, p.control);
        }
        painter.text(cr.center(), Align2::CENTER_CENTER, size_letter(s), theme::font_semibold(9.5), if ok { p.text2 } else { p.text3.gamma_multiply(0.5) });
        cx += 19.0;
    }
    if count > 0 {
        let t = format!("{count} added");
        let g = ui.fonts_mut(|f| f.layout_no_wrap(t, theme::font_semibold(10.5), p.accent));
        let br = Rect::from_min_size(pos2(rect.right() - 12.0 - g.size().x - 12.0, rect.top() + 12.0), vec2(g.size().x + 12.0, 18.0));
        painter.rect_filled(br, 9.0, p.accent_soft);
        painter.galley(br.center() - g.size() / 2.0, g, p.accent);
    } else if hovered {
        painter.text(pos2(rect.right() - 14.0, rect.top() + 14.0), Align2::RIGHT_TOP, glyph::PLUS_CIRCLE, theme::icon_font(18.0), p.accent);
    }
    let desc = ui.fonts_mut(|f| f.layout(info.description.to_string(), theme::font(12.0), p.text2, w - 24.0));
    let dy = ir.bottom() + 10.0;
    painter.with_clip_rect(Rect::from_min_max(pos2(rect.left(), dy), pos2(rect.right(), rect.bottom() - 30.0))).galley(pos2(rect.left() + 12.0, dy), desc, p.text2);
    // Footer: permission / network note, or "No permissions needed".
    let (g, note) = match info.permission_note {
        Some(n) => (icon::SHIELD_CHECK, short_note(n)),
        None => (icon::CHECK_CIRCLE, "Works right away".to_string()),
    };
    let fy = rect.bottom() - 18.0;
    painter.text(pos2(rect.left() + 12.0, fy), Align2::LEFT_CENTER, g, theme::icon_font(12.5), p.text3);
    let ng = ui.fonts_mut(|f| f.layout_no_wrap(note, theme::font(11.5), p.text3));
    let clip = Rect::from_min_max(pos2(rect.left() + 28.0, fy - 10.0), pos2(rect.right() - 10.0, fy + 10.0));
    painter.with_clip_rect(clip).galley(pos2(rect.left() + 28.0, fy - ng.size().y / 2.0), ng, p.text3);
    if let Some(n) = info.permission_note {
        return resp.on_hover_text(n);
    }
    resp
}

/// The first clause of a permission note, for the gallery footer.
fn short_note(n: &str) -> String {
    let lower = n.to_lowercase();
    if lower.contains("calendar access") {
        "Asks for Calendar access".into()
    } else if lower.contains("reminders access") {
        "Asks for Reminders access".into()
    } else if lower.contains("camera access") {
        "Asks for Camera access".into()
    } else if lower.contains("automation") {
        "May ask for Automation access".into()
    } else if lower.contains("open-meteo") {
        "Uses open-meteo.com".into()
    } else if lower.starts_with("runs the command") {
        "Runs your command".into()
    } else if lower.starts_with("runs the plugin") {
        "Runs your plugins".into()
    } else if lower.starts_with("fetches the url") {
        "Fetches your URL".into()
    } else if lower.contains("shortcuts tool") {
        "Runs your shortcuts".into()
    } else if lower.contains("no permission needed") {
        "No permission needed".into()
    } else if lower.contains("audio control") {
        "Uses PowerMac's Sound control".into()
    } else {
        n.split(['.', ';']).next().unwrap_or(n).to_string()
    }
}

/// What a Script / Web / xbar card shows, drawn like the notch draws it.
fn notch_card_preview(ui: &mut Ui, size: WidgetSize, kind: WidgetKind, title: &str, symbol: &str, content: Option<&WidgetPreview>) {
    use notch_shelf::widgets::layout::CARD_H;
    let p = pal(ui);
    let cw = size.width() as f32;
    let ch = CARD_H as f32;
    let pad = 14.0;
    let (outer, _) = ui.allocate_exact_size(vec2(cw + 2.0 * pad, ch + 2.0 * pad), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(outer, 22.0, Color32::BLACK);
    let r = Rect::from_min_size(outer.min + vec2(pad, pad), vec2(cw, ch));
    painter.rect_filled(r, 18.0, Color32::from_gray(30));
    let white = Color32::from_gray(238);
    let gray = Color32::from_gray(150);
    let inner = r.shrink(12.0);
    let pc = painter.with_clip_rect(r.shrink(2.0));
    let Some(c) = content else {
        pc.text(r.center(), Align2::CENTER_CENTER, if kind == WidgetKind::Web { "Not fetched yet" } else { "Not run yet" }, theme::font(12.0), gray);
        let _ = p;
        return;
    };
    let symbol = c.symbol.clone().unwrap_or_else(|| if symbol.trim().is_empty() { notch_shelf::kind_info(kind).sf_symbol.to_string() } else { symbol.to_string() });
    let head = if c.title.trim().is_empty() || c.value.is_none() { title.to_string() } else { c.title.clone() };
    pc.text(inner.left_top(), Align2::LEFT_TOP, sf_glyph(&symbol), theme::icon_font(14.0), gray);
    let hg = ui.fonts_mut(|f| f.layout_no_wrap(head, theme::font_semibold(11.5), gray));
    pc.galley(inner.left_top() + vec2(20.0, 1.0), hg, gray);
    if !c.error.is_empty() {
        pc.text(inner.left_top() + vec2(0.0, 26.0), Align2::LEFT_TOP, icon::WARNING_CIRCLE, theme::icon_font(15.0), Color32::from_rgb(0xFF, 0x6B, 0x5F));
        let eg = ui.fonts_mut(|f| f.layout(c.error.clone(), theme::font(11.5), Color32::from_rgb(0xFF, 0x9A, 0x8F), inner.width() - 22.0));
        pc.galley(inner.left_top() + vec2(22.0, 26.0), eg, white);
        return;
    }
    let mut y = inner.top() + 24.0;
    if let Some(v) = &c.value {
        let vg = ui.fonts_mut(|f| f.layout_no_wrap(v.clone(), theme::font_semibold(if size == WidgetSize::Small { 24.0 } else { 28.0 }), white));
        let vs = vg.size();
        pc.galley(pos2(inner.left(), y), vg, white);
        if !c.unit.is_empty() {
            pc.text(pos2(inner.left() + vs.x + 3.0, y + vs.y - 6.0), Align2::LEFT_BOTTOM, &c.unit, theme::font_semibold(14.0), gray);
        }
        if let Some(pr) = c.progress {
            if size == WidgetSize::Small {
                let bar = Rect::from_min_size(pos2(inner.left(), inner.bottom() - 6.0), vec2(inner.width(), 5.0));
                pc.rect_filled(bar, 2.5, Color32::from_gray(60));
                pc.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * pr.clamp(0.0, 1.0) as f32, 5.0)), 2.5, p.accent);
            } else {
                let center = pos2(inner.right() - 30.0, inner.center().y + 6.0);
                gauge_ring(&pc, center, 26.0, pr as f32, p.accent);
            }
        }
        y += vs.y + 2.0;
        if !c.subtitle.is_empty() && size != WidgetSize::Small || (size == WidgetSize::Small && c.progress.is_none() && !c.subtitle.is_empty()) {
            let sg = ui.fonts_mut(|f| f.layout(c.subtitle.clone(), theme::font(11.5), gray, inner.width() - 70.0));
            pc.galley(pos2(inner.left(), y), sg, gray);
        }
    } else if !c.items.is_empty() {
        for it in c.items.iter().take(if size == WidgetSize::Small { 3 } else { 4 }) {
            pc.circle_filled(pos2(inner.left() + 3.0, y + 8.0), 2.0, gray);
            let ig = ui.fonts_mut(|f| f.layout_no_wrap(it.clone(), theme::font(12.0), white));
            pc.galley(pos2(inner.left() + 11.0, y), ig, white);
            y += 17.0;
        }
    } else {
        let body = if c.body.trim().is_empty() { c.title.clone() } else { c.body.clone() };
        let bg = ui.fonts_mut(|f| f.layout(body, theme::font(12.5), white, inner.width()));
        pc.galley(pos2(inner.left(), y), bg, white);
        if !c.subtitle.is_empty() {
            pc.text(pos2(inner.left(), inner.bottom()), Align2::LEFT_BOTTOM, &c.subtitle, theme::font(11.0), gray);
        }
    }
}

fn gauge_ring(painter: &egui::Painter, c: egui::Pos2, r: f32, v: f32, color: Color32) {
    let n = 48;
    let pts = |frac: f32| -> Vec<egui::Pos2> {
        (0..=((n as f32 * frac).ceil() as usize))
            .map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * (i as f32 / n as f32).min(frac);
                c + vec2(a.cos(), a.sin()) * r
            })
            .collect()
    };
    painter.add(egui::Shape::line(pts(1.0), Stroke::new(5.0, Color32::from_gray(60))));
    if v > 0.0 {
        painter.add(egui::Shape::line(pts(v.clamp(0.0, 1.0)), Stroke::new(5.0, color)));
    }
    painter.text(c, Align2::CENTER_CENTER, format!("{:.0}%", v.clamp(0.0, 1.0) * 100.0), theme::font_semibold(11.0), Color32::from_gray(220));
}

fn script_format_help(ui: &mut Ui) {
    let p = pal(ui);
    let ex = |ui: &mut Ui, what: &str, out: &str| {
        ui.label(RichText::new(what).size(12.0).color(p.text2));
        egui::Frame::new().fill(p.control).corner_radius(6.0).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(out).monospace().size(11.5).color(p.text));
        });
        ui.add_space(4.0);
    };
    ex(ui, "Text: the first line is the title, the rest the body.", "Build #482\npassed in 3m 12s");
    ex(ui, "Number: a value with an optional label and unit, and a caption line. Percentages fill the gauge.", "CPU 73%\nall cores");
    ex(ui, "List: one item per line; a tab separates a subtitle, [x] / [ ] are check marks.", "- web-01\tok\n- db-01\tdegraded\n[x] backups");
    ex(ui, "JSON: everything a card can show; actions are a URL, run:COMMAND, copy:TEXT or refresh.", r##"{"title": "Disk", "value": 73, "unit": "%", "subtitle": "142 GB free",
 "sf_symbol": "internaldrive", "color": "#30D158",
 "items": [{"title": "Open Disk Utility", "action": "run:open -a 'Disk Utility'"}]}"##);
    hint(ui, "Automatic uses JSON for an object or array, a number for one or two lines starting with one, and text otherwise. The widget's id is in $POWERMAC_WIDGET_ID.");
}

fn web_template_help(ui: &mut Ui) {
    let p = pal(ui);
    hint(ui, "{{path}} inserts a value from the JSON response: data.items[0].name, list[-1] (from the end), items[*].name (all), prices.length(). A non-JSON response is {{body}}. Filters follow a bar and can be chained:");
    ui.add_space(4.0);
    egui::Grid::new("nw-filters").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
        for (f, what) in [
            ("{{price | round:2}}", "2 decimals (fixed:2 is the same)"),
            ("{{count | int}}", "drop the decimals"),
            ("{{name | upper}}", "upper / lower / trim"),
            ("{{city | default:Unknown}}", "when missing or empty"),
            ("{{tags | join: · }}", "join an array (\", \" by default)"),
            ("{{items | len}}", "length of an array, object or text"),
            ("{{list | first}}", "first / last element"),
            ("{{ratio | percent:1}}", "0.425 → 42.5%"),
            ("{{size | bytes}}", "1536000 → 1.5 MB"),
            ("{{temp | abs}}", "absolute value"),
            ("{{eta | prefix:ETA }}", "prefix / suffix, only when not empty"),
            ("{{updated | date}}", "Unix time or ISO date → \u{201c}Oct 26, 14:05\u{201d} (date:short, time)"),
        ] {
            ui.label(RichText::new(f).monospace().size(11.5).color(p.text));
            ui.label(RichText::new(what).size(12.0).color(p.text2));
            ui.end_row();
        }
    });
}

// ---------------------------------------------------------------------------
// The generic settings form

struct FormCtx<'a> {
    kind: WidgetKind,
    id: String,
    secrets_shown: &'a mut HashSet<String>,
    /// `None` while loading.
    shortcuts: Option<&'a [String]>,
    reminder_lists: Vec<String>,
    apps: &'a [String],
    pick: &'a mut Option<(String, &'static str, String)>,
    choosing: &'a HashSet<String>,
}

/// Set `obj[path]` where path is "name" or "list[2].field".
fn set_at_path(obj: &mut Map<String, Value>, path: &str, v: Value) {
    if let Some((name, rest)) = path.split_once('[') {
        let Some((idx, field)) = rest.split_once("].") else { return };
        let Ok(i) = idx.parse::<usize>() else { return };
        if let Some(Value::Array(a)) = obj.get_mut(name) {
            if let Some(Value::Object(o)) = a.get_mut(i) {
                o.insert(field.to_string(), v);
            }
        }
    } else {
        obj.insert(path.to_string(), v);
    }
}

/// Render `fields` editing `obj`; `prefix` is the path of the object
/// ("" for the settings, "items[2]" for a list item).
fn settings_form(ui: &mut Ui, ctx: &mut FormCtx, fields: &[SettingField], obj: &mut Map<String, Value>, prefix: &str, errors: &[FieldError]) {
    for f in fields {
        if !field_shown(f, fields, obj) {
            continue;
        }
        let path = if prefix.is_empty() { f.name.to_string() } else { format!("{prefix}.{}", f.name) };
        let errs = errors_for(errors, &path);
        ui.push_id(("nw-field", &path), |ui| {
            field_control(ui, ctx, f, obj, &path);
            if !errs.is_empty() {
                for e in errs.iter().take(4) {
                    error_label(ui, &error_text(f, e, &path));
                }
                ui.add_space(6.0);
            }
        });
    }
}

fn help(f: &SettingField) -> Option<&'static str> {
    (!f.help.is_empty()).then_some(f.help)
}

fn field_control(ui: &mut Ui, ctx: &mut FormCtx, f: &SettingField, obj: &mut Map<String, Value>, path: &str) {
    let kind = ctx.kind;
    match f.ty {
        FieldType::Bool => {
            let mut b = match obj.get(f.name) {
                Some(Value::Bool(b)) => *b,
                Some(v) => text_of(Some(v)) == "true",
                None => f.default.as_bool().unwrap_or(false),
            };
            if toggle_row(ui, f.label, help(f), &mut b).changed() {
                obj.insert(f.name.into(), json!(b));
            }
        }
        FieldType::Number => {
            let mut n = obj.get(f.name).and_then(|v| v.as_f64().or_else(|| text_of(Some(v)).trim().parse().ok())).unwrap_or(f.default.as_f64().unwrap_or(0.0));
            let label = if kind == WidgetKind::Xbar && f.name == "interval" && n == 0.0 { format!("{} (from the file name)", f.label) } else { f.label.to_string() };
            row(ui, &label, help(f), |ui| {
                let mut dv = egui::DragValue::new(&mut n).speed(f.step.unwrap_or(1.0).max(0.01));
                if let (Some(lo), Some(hi)) = (f.min, f.max) {
                    dv = dv.range(lo..=hi);
                }
                if !f.unit.is_empty() {
                    dv = dv.suffix(format!(" {}", f.unit));
                }
                if f.step.is_some_and(|s| s >= 1.0) {
                    dv = dv.max_decimals(0);
                }
                if ui.add_sized(vec2(96.0, 24.0), dv).changed() {
                    obj.insert(f.name.into(), num_value(n));
                }
            });
        }
        FieldType::Choice => {
            let cur = obj.get(f.name).map(|v| text_of(Some(v))).unwrap_or_else(|| text_of(Some(&f.default)));
            row(ui, f.label, help(f), |ui| {
                let total: usize = f.choices.iter().map(|c| c.label.chars().count()).sum();
                if f.choices.len() <= 3 && total <= 30 {
                    let mut sel = f.choices.iter().position(|c| c.value == cur).unwrap_or(0);
                    let items: Vec<(usize, &str)> = f.choices.iter().enumerate().map(|(i, c)| (i, c.label)).collect();
                    if segmented(ui, &mut sel, &items).changed() {
                        obj.insert(f.name.into(), json!(f.choices[sel].value));
                    }
                } else {
                    let shown = f.choices.iter().find(|c| c.value == cur).map(|c| c.label).unwrap_or("Choose…");
                    egui::ComboBox::from_id_salt(("nw-choice", path)).selected_text(shown).width(220.0).height(360.0).show_ui(ui, |ui| {
                        for c in &f.choices {
                            if ui.selectable_label(c.value == cur, c.label).clicked() {
                                obj.insert(f.name.into(), json!(c.value));
                            }
                        }
                    });
                }
            });
        }
        FieldType::Secret => {
            let key = format!("{}/{}", ctx.id, path);
            let mut show = ctx.secrets_shown.contains(&key);
            let mut s = text_of(obj.get(f.name));
            let mut toggled = false;
            row(ui, f.label, help(f), |ui| {
                if ui.add(egui::TextEdit::singleline(&mut s).password(!show).hint_text(if f.placeholder.is_empty() { "Not set" } else { f.placeholder }).desired_width(230.0).font(egui::TextStyle::Monospace)).changed() {
                    obj.insert(f.name.into(), json!(s));
                }
                if icon_button(ui, if show { icon::EYE_SLASH } else { icon::EYE }, if show { "Hide" } else { "Show" }).clicked() {
                    toggled = true;
                }
            });
            if toggled {
                show = !show;
                if show {
                    ctx.secrets_shown.insert(key);
                } else {
                    ctx.secrets_shown.remove(&key);
                }
            }
        }
        FieldType::Text | FieldType::Url | FieldType::Path => {
            let mut s = text_of(obj.get(f.name));
            if obj.get(f.name).is_none() {
                s = text_of(Some(&f.default));
            }
            let deferred = commit_on_blur(kind, f);
            let mono = f.multiline || matches!(f.ty, FieldType::Path) || matches!(f.name, "template" | "valuePath" | "listPath" | "itemTemplate" | "clickTarget" | "zone" | "subtitle" | "openUrl" | "interpreterPath");
            let id = ui.scope_id().with(("nw-text", path));
            let hint_text = if f.placeholder.is_empty() { "" } else { f.placeholder };
            if f.multiline {
                // A code area: label and help above, full width.
                block(ui, |ui| {
                    let p = pal(ui);
                    ui.label(RichText::new(f.label).color(p.text));
                    if let Some(h) = help(f) {
                        ui.label(RichText::new(h).size(12.0).color(p.text2));
                    }
                    ui.add_space(2.0);
                    let width = ui.available_width();
                    let (committed, _) = deferred_text(ui, id, &mut s, TextOpts { hint: hint_text, width, mono: true, rows: 3 });
                    if committed {
                        obj.insert(f.name.into(), json!(s));
                    }
                    if deferred_dirty(ui, id, &s) {
                        ui.label(RichText::new("Applied when you click outside the field.").size(11.5).color(p.text3));
                    }
                });
                return;
            }
            let mut pick_now = false;
            row(ui, f.label, help(f), |ui| {
                let w = if matches!(f.ty, FieldType::Path) { 260.0 } else if matches!(f.ty, FieldType::Url) { 380.0 } else if matches!(f.name, "template" | "subtitle" | "openUrl" | "itemTemplate" | "clickTarget") { 320.0 } else { 230.0 };
                let o = TextOpts { hint: hint_text, width: w, mono, rows: 0 };
                if deferred {
                    let (committed, _) = deferred_text(ui, id, &mut s, o);
                    if committed {
                        obj.insert(f.name.into(), json!(s));
                    }
                } else if ui.add(text_edit(&mut s, o)).changed() {
                    obj.insert(f.name.into(), json!(s));
                }
                if matches!(f.ty, FieldType::Path) && cfg!(target_os = "macos") {
                    let key = format!("field:{}:{}", ctx.id, path);
                    if ui.add_enabled(!ctx.choosing.contains(&key), egui::Button::new("Choose…")).clicked() {
                        pick_now = true;
                    }
                }
            });
            if pick_now {
                let what = if kind == WidgetKind::Xbar || f.name == "workingDir" { "folder" } else { "file" };
                let what = if kind == WidgetKind::Xbar { "any" } else { what };
                *ctx.pick = Some((format!("field:{}:{}", ctx.id, path), if what == "any" { "folder" } else { what }, format!("Choose: {}", f.label)));
            }
            if matches!(f.ty, FieldType::Url) {
                let t = s.trim();
                if !t.is_empty() && !(t.starts_with("https://") || t.starts_with("http://")) {
                    warn_label(ui, "Starts with https:// (or http:// with “Allow plain HTTP” on).");
                    ui.add_space(6.0);
                }
            }
        }
        FieldType::List => list_field(ui, ctx, f, obj, path),
    }
}

/// Suggestions for a list of names (shortcuts, reminder lists, controls).
fn list_suggestions(ctx: &FormCtx, f: &SettingField) -> Option<Vec<(String, String)>> {
    match (ctx.kind, f.name) {
        (WidgetKind::Shortcuts, "shortcuts") => ctx.shortcuts.map(|l| l.iter().map(|s| (s.clone(), s.clone())).collect()),
        (WidgetKind::Reminders, "lists") if !ctx.reminder_lists.is_empty() => Some(ctx.reminder_lists.iter().map(|s| (s.clone(), s.clone())).collect()),
        (WidgetKind::QuickToggles, "controls") => Some(QUICK_CONTROLS.iter().map(|(id, l)| (id.to_string(), l.to_string())).collect()),
        _ => None,
    }
}

fn list_field(ui: &mut Ui, ctx: &mut FormCtx, f: &SettingField, obj: &mut Map<String, Value>, path: &str) {
    let mut items: Vec<Value> = match obj.get(f.name) {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => s.lines().filter(|l| !l.trim().is_empty()).map(|l| json!(l.trim())).collect(),
        None => f.default.as_array().cloned().unwrap_or_default(),
        _ => vec![],
    };
    let before = items.clone();
    let p = pal(ui);
    block(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(f.label).color(p.text));
            ui.label(RichText::new(format!("{}", items.len())).size(11.5).color(p.text3));
        });
        if let Some(h) = help(f) {
            ui.add(egui::Label::new(RichText::new(h).size(12.0).color(p.text2)).wrap());
        }
        ui.add_space(4.0);
        let mut remove = None;
        let mut mv: Option<(usize, usize)> = None;
        let n = items.len();
        if !f.item_fields.is_empty() {
            // A list of objects: one compact panel per item.
            for (i, it) in items.iter_mut().enumerate() {
                if !it.is_object() {
                    *it = json!({});
                }
                ui.push_id(("nw-item", i), |ui| {
                    egui::Frame::new().fill(p.control.gamma_multiply(0.55)).stroke(Stroke::new(1.0, p.separator)).corner_radius(9.0).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{}", i + 1)).font(theme::font_semibold(12.0)).color(p.text3));
                            let o = it.as_object_mut().expect("object");
                            let item_path = format!("{path}[{i}]");
                            let fw = (ui.available_width() - 92.0).max(200.0);
                            ui.allocate_ui_with_layout(vec2(fw, 0.0), Layout::left_to_right(Align::Min).with_main_wrap(true), |ui| {
                                ui.spacing_mut().item_spacing = vec2(10.0, 6.0);
                                for sf in &f.item_fields {
                                    if !field_shown(sf, &f.item_fields, o) {
                                        continue;
                                    }
                                    compact_field(ui, ctx, sf, o, &format!("{item_path}.{}", sf.name));
                                }
                            });
                            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                if icon_button(ui, icon::TRASH, "Remove").clicked() {
                                    remove = Some(i);
                                }
                                if ui.add_enabled(i + 1 < n, egui::Button::new(theme::icon_text(icon::ARROW_DOWN).size(13.0)).min_size(vec2(24.0, 24.0))).on_hover_text("Move down").clicked() {
                                    mv = Some((i, i + 1));
                                }
                                if ui.add_enabled(i > 0, egui::Button::new(theme::icon_text(icon::ARROW_UP).size(13.0)).min_size(vec2(24.0, 24.0))).on_hover_text("Move up").clicked() {
                                    mv = Some((i, i - 1));
                                }
                            });
                        });
                    });
                });
                ui.add_space(2.0);
            }
            ui.add_space(4.0);
            if icon_text_button(ui, icon::PLUS, &format!("Add {}", item_noun(f).to_lowercase())).clicked() {
                let mut o = Map::new();
                for sf in &f.item_fields {
                    o.insert(sf.name.into(), sf.default.clone());
                }
                items.push(Value::Object(o));
            }
        } else {
            let suggestions = list_suggestions(ctx, f);
            let label_of = |v: &str| suggestions.as_ref().and_then(|s| s.iter().find(|(id, _)| id == v)).map(|(_, l)| l.clone());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for (i, it) in items.iter_mut().enumerate() {
                    ui.push_id(("nw-chip", i), |ui| {
                        egui::Frame::new().fill(p.control).corner_radius(14.0).inner_margin(egui::Margin { left: 10, right: 4, top: 2, bottom: 2 }).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                if f.item_type == FieldType::Number {
                                    let mut x = it.as_f64().unwrap_or(0.0);
                                    if ui.add(egui::DragValue::new(&mut x).range(0.0..=1440.0).speed(1.0).max_decimals(1).suffix(if f.name == "presets" { " min" } else { "" })).changed() {
                                        *it = num_value(x);
                                    }
                                } else {
                                    let raw = text_of(Some(it));
                                    match label_of(&raw) {
                                        Some(l) if l != raw => {
                                            ui.label(RichText::new(l).size(12.5).color(p.text)).on_hover_text(&raw);
                                        }
                                        _ => {
                                            let mut s = raw.clone();
                                            let w = (ui.fonts_mut(|f| f.layout_no_wrap(s.clone(), theme::font(12.5), p.text)).size().x + 12.0).clamp(40.0, 260.0);
                                            if ui.add(egui::TextEdit::singleline(&mut s).frame(egui::Frame::NONE).desired_width(w)).changed() {
                                                *it = json!(s);
                                            }
                                        }
                                    }
                                }
                                if ui.add(egui::Button::new(theme::icon_text(icon::X).size(11.0).color(p.text2)).frame(false).min_size(vec2(18.0, 18.0))).on_hover_text("Remove").clicked() {
                                    remove = Some(i);
                                }
                            });
                        });
                    });
                }
                if items.is_empty() {
                    let empty = match (ctx.kind, f.name) {
                        (WidgetKind::Calendar, _) => "All calendars",
                        (WidgetKind::Reminders, _) => "All lists",
                        (WidgetKind::Bluetooth, _) => "Every device",
                        (WidgetKind::QuickToggles, _) => "All controls",
                        _ => "None yet",
                    };
                    ui.label(RichText::new(empty).size(12.5).color(p.text3));
                }
            });
            ui.add_space(4.0);
            // Adding: a menu of suggestions and / or a text field.
            let add_id = ui.scope_id().with(("nw-add", path));
            let mut new_text: String = ui.data(|d| d.get_temp(add_id)).unwrap_or_default();
            let mut add: Option<Value> = None;
            ui.horizontal(|ui| {
                if f.item_type == FieldType::Number {
                    if icon_text_button(ui, icon::PLUS, &format!("Add {}", item_noun(f).to_lowercase())).clicked() {
                        let last = items.last().and_then(|v| v.as_f64()).unwrap_or(5.0);
                        add = Some(num_value((last + 5.0).min(1440.0)));
                    }
                    return;
                }
                let loading = ctx.kind == WidgetKind::Shortcuts && f.name == "shortcuts" && ctx.shortcuts.is_none();
                if let Some(sug) = &suggestions {
                    let free: Vec<&(String, String)> = sug.iter().filter(|(id, _)| !items.iter().any(|v| text_of(Some(v)) == *id)).collect();
                    let title = match ctx.kind {
                        WidgetKind::Shortcuts => "Add shortcut",
                        WidgetKind::Reminders => "Add list",
                        _ => "Add",
                    };
                    ui.add_enabled_ui(!free.is_empty(), |ui| {
                        ui.menu_button(theme::icon_job(icon::PLUS, title, 13.0, p.text), |ui| {
                            ui.set_min_width(220.0);
                            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                for (id, l) in &free {
                                    if ui.button(l.as_str()).clicked() {
                                        add = Some(json!(id));
                                        ui.close();
                                    }
                                }
                            });
                        });
                    });
                    if sug.is_empty() && ctx.kind == WidgetKind::Shortcuts {
                        ui.label(RichText::new("No shortcuts found; type a name.").size(12.0).color(p.text3));
                    }
                } else if loading {
                    ui.spinner();
                    ui.label(RichText::new("Reading your shortcuts…").size(12.0).color(p.text3));
                }
                let hint_text = match (ctx.kind, f.name) {
                    (WidgetKind::Shortcuts, _) => "Shortcut name",
                    (WidgetKind::Calendar, _) => "Calendar name",
                    (WidgetKind::Reminders, _) => "List name",
                    (WidgetKind::Bluetooth, _) => "Device name",
                    (WidgetKind::Web, "headers") => "Name: value",
                    _ => "Add…",
                };
                let r = ui.add(egui::TextEdit::singleline(&mut new_text).hint_text(hint_text).desired_width(200.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.add_enabled(!new_text.trim().is_empty(), egui::Button::new("Add")).clicked() || enter) && !new_text.trim().is_empty() {
                    add = Some(json!(new_text.trim()));
                    new_text.clear();
                }
            });
            ui.data_mut(|d| d.insert_temp(add_id, new_text));
            if let Some(v) = add {
                items.push(v);
            }
        }
        if let Some(i) = remove {
            items.remove(i);
        }
        if let Some((a, b)) = mv {
            items.swap(a, b);
        }
    });
    if items != before {
        obj.insert(f.name.into(), Value::Array(items));
    }
}

/// A small labeled control for a field of a list item (label above).
fn compact_field(ui: &mut Ui, ctx: &mut FormCtx, f: &SettingField, o: &mut Map<String, Value>, path: &str) {
    let p = pal(ui);
    let col = match (f.ty, f.name) {
        (FieldType::Choice, _) => 116.0,
        (FieldType::Number | FieldType::Bool, _) => 84.0,
        (_, "target") => 204.0,
        (_, "zone") => 212.0,
        (_, "symbol") => 92.0,
        _ => 112.0,
    };
    ui.allocate_ui_with_layout(vec2(col, 46.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(col);
        ui.spacing_mut().item_spacing.y = 2.0;
        let mut l = ui.label(RichText::new(f.label).size(11.5).color(p.text2));
        if !f.help.is_empty() {
            l = l.on_hover_text(f.help);
        }
        let _ = l;
        match f.ty {
            FieldType::Choice => {
                let cur = o.get(f.name).map(|v| text_of(Some(v))).unwrap_or_else(|| text_of(Some(&f.default)));
                let shown = f.choices.iter().find(|c| c.value == cur).map(|c| c.label).unwrap_or("Choose…");
                egui::ComboBox::from_id_salt(("nw-cchoice", path)).selected_text(shown).width(col - 8.0).show_ui(ui, |ui| {
                    for c in &f.choices {
                        if ui.selectable_label(c.value == cur, c.label).clicked() {
                            o.insert(f.name.into(), json!(c.value));
                        }
                    }
                });
            }
            FieldType::Bool => {
                let mut b = o.get(f.name).and_then(|v| v.as_bool()).unwrap_or(f.default.as_bool().unwrap_or(false));
                if toggle_labeled(ui, &mut b, f.label).changed() {
                    o.insert(f.name.into(), json!(b));
                }
            }
            FieldType::Number => {
                let mut n = o.get(f.name).and_then(|v| v.as_f64()).unwrap_or(f.default.as_f64().unwrap_or(0.0));
                let mut dv = egui::DragValue::new(&mut n);
                if let (Some(lo), Some(hi)) = (f.min, f.max) {
                    dv = dv.range(lo..=hi);
                }
                if ui.add_sized(vec2(80.0, 24.0), dv).changed() {
                    o.insert(f.name.into(), num_value(n));
                }
            }
            _ => {
                let mut s = o.get(f.name).map(|v| text_of(Some(v))).unwrap_or_else(|| text_of(Some(&f.default)));
                let item_kind = text_of(o.get("kind"));
                let has_menu = (ctx.kind == WidgetKind::Launcher && f.name == "target") || (ctx.kind == WidgetKind::WorldClock && f.name == "zone");
                let w = if has_menu { col - 32.0 } else { col - 4.0 };
                let id = ui.scope_id().with(("nw-ctext", path));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let mono = matches!(f.name, "zone" | "symbol") || (f.name == "target" && item_kind == "command");
                    let city;
                    let hint = if ctx.kind == WidgetKind::WorldClock && f.name == "label" {
                        city = text_of(o.get("zone")).rsplit('/').next().unwrap_or("").replace('_', " ");
                        city.as_str()
                    } else {
                        f.placeholder
                    };
                    let opts = TextOpts { hint, width: w, mono, rows: 0 };
                    if commit_on_blur(ctx.kind, f) {
                        let (c, _) = deferred_text(ui, id, &mut s, opts);
                        if c {
                            o.insert(f.name.into(), json!(s));
                        }
                    } else if ui.add(text_edit(&mut s, opts)).changed() {
                        o.insert(f.name.into(), json!(s));
                    }
                    // Helpers: apps for launcher apps, zones for clocks.
                    if ctx.kind == WidgetKind::Launcher && f.name == "target" && (item_kind == "app" || item_kind.is_empty()) && !ctx.apps.is_empty() {
                        ui.menu_button(theme::icon_text(icon::CARET_DOWN).size(12.0).color(p.text2), |ui| {
                            ui.set_min_width(220.0);
                            egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                                for a in ctx.apps {
                                    if ui.button(a.as_str()).clicked() {
                                        o.insert(f.name.into(), json!(a));
                                        if text_of(o.get("label")).is_empty() {
                                            // The label follows the app name by default.
                                        }
                                        ui.close();
                                    }
                                }
                            });
                        })
                        .response
                        .on_hover_text("Choose an app");
                    }
                    if ctx.kind == WidgetKind::Launcher && f.name == "target" && matches!(item_kind.as_str(), "file" | "folder") && cfg!(target_os = "macos") {
                        let key = format!("field:{}:{}", ctx.id, path);
                        if ui.add_enabled(!ctx.choosing.contains(&key), egui::Button::new(theme::icon_text(icon::FOLDER_OPEN).size(13.0).color(p.text2)).min_size(vec2(26.0, 24.0))).on_hover_text("Choose…").clicked() {
                            *ctx.pick = Some((key, if item_kind == "folder" { "folder" } else { "file" }, "Choose an item for the launcher".into()));
                        }
                    }
                    if ctx.kind == WidgetKind::WorldClock && f.name == "zone" {
                        ui.menu_button(theme::icon_text(icon::CARET_DOWN).size(12.0).color(p.text2), |ui| {
                            ui.set_min_width(240.0);
                            egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                                for (z, city) in TIME_ZONES {
                                    if ui.button(format!("{city}  ·  {z}")).clicked() {
                                        o.insert("zone".into(), json!(z));
                                        if text_of(o.get("label")).trim().is_empty() {
                                            o.insert("label".into(), json!(city));
                                        }
                                        ui.close();
                                    }
                                }
                            });
                        })
                        .response
                        .on_hover_text("Common time zones");
                    }
                });
            }
        }
    });
}

#[cfg(test)]
impl SettingsApp {
    pub fn nw_select_for_test(&mut self, id: Option<&str>) {
        self.nw.selected = id.map(String::from);
        self.nw.gallery = false;
    }
    pub fn nw_gallery_for_test(&mut self, open: bool) {
        self.nw.gallery = open;
    }
    pub fn nw_page_for_test(&mut self, page: usize) {
        self.nw.page = page;
    }
    pub fn nw_shortcuts_for_test(&mut self, list: Vec<String>) {
        *self.nw.shortcuts.lock().unwrap() = Some(list);
        self.nw.shortcuts_requested = true;
    }
    pub fn nw_test_result_for_test(&mut self, id: &str, r: Result<notch_shelf::WidgetContent, String>) {
        let done = TestRun::Done { result: Box::new(WidgetPreview::from_result(&r)), took: Duration::from_millis(420), at: Instant::now() };
        self.nw.tests.lock().unwrap().insert(id.to_string(), done);
    }
    pub fn nw_apps_for_test(&mut self, apps: Vec<String>) {
        self.nw.apps = Some(apps);
    }
    pub fn nw_io_result_for_test(&mut self, ok: bool, msg: &str, warnings: Vec<String>) {
        self.nw.io_result = Some((ok, msg.into(), warnings));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(sf_glyph("house.fill"), glyph::HOUSE);
        assert_eq!(sf_glyph("no.such.symbol"), glyph::SQUARE);
        for info in notch_shelf::widget_catalog() {
            assert_ne!(sf_glyph(info.sf_symbol), glyph::SQUARE, "{} has a glyph", info.id);
        }
        for (s, _) in PAGE_SYMBOLS.iter().skip(1) {
            assert_ne!(sf_glyph(s), glyph::SQUARE, "{s}");
        }
        assert_eq!(xbar_interval_text("cpu.5m.sh"), "every 5 min");
        assert_eq!(xbar_interval_text("weather.10s.py"), "every 10 s");
        assert_eq!(xbar_interval_text("plain.sh"), "no interval in the name");
        let mut o = Map::new();
        o.insert("items".into(), json!([{"kind": "app", "target": ""}]));
        set_at_path(&mut o, "items[0].target", json!("Safari"));
        set_at_path(&mut o, "path", json!("~/x"));
        assert_eq!(o["items"][0]["target"], "Safari");
        assert_eq!(o["path"], "~/x");
        // show_if with bool fields given as "true".
        let info = notch_shelf::kind_info(WidgetKind::Script);
        let min = info.fields.iter().find(|f| f.name == "min").unwrap();
        let mut obj = notch_shelf::default_settings(WidgetKind::Script).as_object().unwrap().clone();
        assert!(field_shown(min, &info.fields, &obj));
        obj.insert("gauge".into(), json!(false));
        assert!(!field_shown(min, &info.fields, &obj));
        let target = info.fields.iter().find(|f| f.name == "clickTarget").unwrap();
        assert!(!field_shown(target, &info.fields, &obj));
        obj.insert("click".into(), json!("openUrl"));
        assert!(field_shown(target, &info.fields, &obj));
        // Error texts for list items.
        let clocks = notch_shelf::kind_info(WidgetKind::WorldClock).fields.iter().find(|f| f.name == "clocks").unwrap().clone();
        let e = FieldError { field: "clocks[1].zone".into(), message: "is required".into() };
        assert_eq!(error_text(&clocks, &e, "clocks"), "Clock 2: Time zone is required");
        assert_eq!(errors_for(std::slice::from_ref(&e), "clocks").len(), 1);
        assert!(errors_for(std::slice::from_ref(&e), "clock").is_empty());
        // Titles and summaries.
        let mut w = WidgetInstance::new(WidgetKind::Script);
        assert_eq!(instance_title(&w), "Script");
        assert_eq!(instance_summary(&w), "No command yet");
        w.settings["title"] = json!("Disk");
        w.settings["command"] = json!("df -h /\necho hi");
        assert_eq!((instance_title(&w).as_str(), instance_summary(&w).as_str()), ("Disk", "df -h /"));
        let sc = WidgetInstance::new(WidgetKind::Shortcuts);
        assert_eq!(instance_title(&sc), "Shortcuts");
        assert!(commit_on_blur(WidgetKind::Script, info.fields.iter().find(|f| f.name == "command").unwrap()));
        assert_eq!(short_note("macOS asks for Calendar access the first time the widget is shown."), "Asks for Calendar access");
    }
}
