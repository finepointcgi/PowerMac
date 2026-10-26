//! Widget instances and Home pages: the configurable layout of the Home
//! view.
//!
//! A [`WidgetInstance`] is one card: a kind, a size, the page it lives on
//! and kind-specific settings (a JSON object, typed per kind in
//! [`super::catalog`]). The same kind can be added several times with
//! different settings (two world clocks, three scripts, ...).
//!
//! Older configs stored `widgets` as `[{kind, enabled}]`; [`deserialize_widgets`]
//! accepts both shapes and [`migrate_legacy`] turns the old one into
//! instances without losing the order, the on/off state or the global
//! weather / calendar / now playing settings (they move into the instance
//! settings).

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::layout::{CARD_GAP, LARGE_W, MEDIUM_W, SMALL_W};
use super::{CalendarConfig, NowPlayingConfig, WeatherConfig, WidgetConfig, WidgetKind};

/// Card width class. All cards have the same height; a medium card is two
/// small ones plus the gap, a large one three.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum WidgetSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl WidgetSize {
    pub const ALL: [WidgetSize; 3] = [WidgetSize::Small, WidgetSize::Medium, WidgetSize::Large];

    /// Card width in points.
    pub fn width(self) -> f64 {
        match self {
            WidgetSize::Small => SMALL_W,
            WidgetSize::Medium => MEDIUM_W,
            WidgetSize::Large => LARGE_W,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WidgetSize::Small => "Small",
            WidgetSize::Medium => "Medium",
            WidgetSize::Large => "Large",
        }
    }

    /// Grid units (1, 2, 3).
    pub fn units(self) -> usize {
        match self {
            WidgetSize::Small => 1,
            WidgetSize::Medium => 2,
            WidgetSize::Large => 3,
        }
    }
}

const _: () = assert!(MEDIUM_W == 2.0 * SMALL_W + CARD_GAP && LARGE_W == 3.0 * SMALL_W + 2.0 * CARD_GAP);

/// A named Home page ("Work", "Media", "Dev").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WidgetPage {
    pub name: String,
    /// Optional SF Symbol shown in the page tabs instead of the name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub symbol: String,
}

impl Default for WidgetPage {
    fn default() -> Self {
        WidgetPage { name: "Home".into(), symbol: String::new() }
    }
}

impl WidgetPage {
    pub fn named(name: &str) -> Self {
        WidgetPage { name: name.into(), symbol: String::new() }
    }

    pub fn defaults() -> Vec<WidgetPage> {
        vec![WidgetPage::default()]
    }
}

/// One widget card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetInstance {
    /// Unique id (a UUID for new instances). Also names per-instance files
    /// (the note's text, caches).
    pub id: String,
    pub kind: WidgetKind,
    #[serde(default)]
    pub size: WidgetSize,
    /// Index into the config's `pages` (out-of-range indexes show on the
    /// last page).
    #[serde(default)]
    pub page: usize,
    /// Off: kept with its settings but not shown.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Kind-specific settings (see [`super::catalog::widget_catalog`]).
    #[serde(default = "empty_object")]
    pub settings: Value,
}

fn yes() -> bool {
    true
}

pub fn empty_object() -> Value {
    Value::Object(Map::new())
}

impl WidgetInstance {
    /// A new instance with a fresh id, the kind's default size and default
    /// settings.
    pub fn new(kind: WidgetKind) -> Self {
        let info = super::catalog::kind_info(kind);
        WidgetInstance { id: new_id(), kind, size: info.default_size, page: 0, enabled: true, settings: super::catalog::default_settings(kind) }
    }

    pub fn on_page(mut self, page: usize) -> Self {
        self.page = page;
        self
    }

    pub fn sized(mut self, size: WidgetSize) -> Self {
        self.size = size;
        self
    }

    pub fn with_settings(mut self, settings: Value) -> Self {
        self.settings = settings;
        self
    }

    /// The size, moved to the nearest size the kind allows.
    pub fn effective_size(&self) -> WidgetSize {
        let allowed = &super::catalog::kind_info(self.kind).sizes;
        if allowed.is_empty() || allowed.contains(&self.size) {
            return self.size;
        }
        allowed.iter().copied().min_by_key(|s| (s.units() as i32 - self.size.units() as i32).abs()).unwrap()
    }
}

/// A random version 4 UUID ("f47ac10b-58cc-4372-a567-0e02b2c3d479").
pub fn new_id() -> String {
    let mut b = random_bytes();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format_uuid(&b)
}

fn format_uuid(b: &[u8; 16]) -> String {
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// 16 random bytes: the OS random source where there is one, else hashed
/// time / address / counter entropy (ids only need to be unique).
fn random_bytes() -> [u8; 16] {
    let mut b = [0u8; 16];
    #[cfg(unix)]
    {
        use std::io::Read;
        if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_ok() {
            return b;
        }
    }
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    for half in 0..2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(N.fetch_add(1, Ordering::Relaxed));
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_usize(&b as *const _ as usize);
        b[half * 8..half * 8 + 8].copy_from_slice(&h.finish().to_le_bytes());
    }
    b
}

/// Stable id for an instance migrated from the old `[{kind, enabled}]`
/// list (so reloading an old config gives the same ids every time).
pub fn legacy_id(kind: WidgetKind) -> String {
    // FNV-1a of the kind name, spread over 16 bytes, formatted as a v4 UUID.
    let name = serde_json::to_string(&kind).unwrap_or_default();
    let mut b = [0u8; 16];
    let mut h: u64 = 0xcbf29ce484222325;
    for (i, slot) in b.iter_mut().enumerate() {
        for c in name.bytes().chain([i as u8]) {
            h ^= c as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        *slot = (h >> 24) as u8;
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format_uuid(&b)
}

/// The default Home view of a fresh install.
pub fn default_instances() -> Vec<WidgetInstance> {
    let mk = |kind: WidgetKind, size: WidgetSize| WidgetInstance {
        id: legacy_id(kind),
        kind,
        size,
        page: 0,
        enabled: true,
        settings: super::catalog::default_settings(kind),
    };
    vec![
        mk(WidgetKind::NowPlaying, WidgetSize::Medium),
        mk(WidgetKind::Battery, WidgetSize::Small),
        mk(WidgetKind::Timer, WidgetSize::Medium),
        mk(WidgetKind::QuickToggles, WidgetSize::Medium),
    ]
}

/// Size an old (pre-instances) card of `kind` gets: close to its old width.
fn legacy_size(kind: WidgetKind) -> WidgetSize {
    match kind {
        WidgetKind::Weather | WidgetKind::Battery | WidgetKind::Mirror => WidgetSize::Small,
        _ => WidgetSize::Medium,
    }
}

/// Global settings of old configs that now live in the instances.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LegacySettings {
    pub weather: WeatherConfig,
    pub calendar: CalendarConfig,
    pub now_playing: NowPlayingConfig,
}

impl LegacySettings {
    /// Instance settings for a migrated card of `kind`.
    pub fn settings_for(&self, kind: WidgetKind) -> Value {
        let mut v = super::catalog::default_settings(kind);
        let extra = match kind {
            WidgetKind::Weather => json!({ "location": self.weather.location, "units": self.weather.units }),
            WidgetKind::Calendar => json!({ "days": self.calendar.days, "calendars": self.calendar.calendars }),
            WidgetKind::NowPlaying => json!({ "useApplescript": self.now_playing.use_applescript }),
            _ => Value::Null,
        };
        if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
            o.extend(e);
        }
        v
    }
}

/// Old `[{kind, enabled}]` list → instances, in the same order with the
/// same on/off state, on the first page. Unknown kinds and duplicates are
/// dropped (old builds ignored them too).
pub fn migrate_legacy(list: &[WidgetConfig], legacy: &LegacySettings) -> Vec<WidgetInstance> {
    let mut out: Vec<WidgetInstance> = vec![];
    for w in list {
        if w.kind == WidgetKind::Unknown || out.iter().any(|o| o.kind == w.kind) {
            continue;
        }
        out.push(WidgetInstance { id: legacy_id(w.kind), kind: w.kind, size: legacy_size(w.kind), page: 0, enabled: w.enabled, settings: legacy.settings_for(w.kind) });
    }
    out
}

/// What a `widgets` array held when it was read.
#[derive(Clone, Debug, PartialEq)]
pub enum WidgetList {
    Instances(Vec<WidgetInstance>),
    /// The old format, still to be migrated with the legacy settings.
    Legacy(Vec<WidgetConfig>),
}

/// Parse a `widgets` JSON array in either format. Entries with an `id` are
/// instances; entries without one are old `{kind, enabled}` items (the
/// whole list is then treated as old). Malformed entries are skipped.
pub fn parse_widget_list(v: &Value) -> WidgetList {
    let Some(arr) = v.as_array() else { return WidgetList::Instances(vec![]) };
    let legacy = !arr.is_empty() && arr.iter().all(|e| e.get("id").is_none());
    if legacy {
        return WidgetList::Legacy(arr.iter().filter_map(|e| serde_json::from_value::<WidgetConfig>(e.clone()).ok()).collect());
    }
    let mut out: Vec<WidgetInstance> = vec![];
    for e in arr {
        let inst = match serde_json::from_value::<WidgetInstance>(e.clone()) {
            Ok(i) => i,
            Err(_) => match serde_json::from_value::<WidgetConfig>(e.clone()) {
                // An old entry mixed in: give it an id.
                Ok(w) => WidgetInstance { id: legacy_id(w.kind), kind: w.kind, size: legacy_size(w.kind), page: 0, enabled: w.enabled, settings: super::catalog::default_settings(w.kind) },
                Err(_) => continue,
            },
        };
        if inst.id.is_empty() || out.iter().any(|o| o.id == inst.id) {
            continue;
        }
        out.push(inst);
    }
    WidgetList::Instances(out)
}

/// serde helper for a `widgets` field that may be in the old format; old
/// lists are migrated with default settings (use [`parse_widget_list`] +
/// [`migrate_legacy`] when the old global settings are at hand).
pub fn deserialize_widgets<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<WidgetInstance>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match parse_widget_list(&v) {
        WidgetList::Instances(v) => v,
        WidgetList::Legacy(l) => migrate_legacy(&l, &LegacySettings::default()),
    })
}

// ----- pages ---------------------------------------------------------------

/// Instances that show on `page`, in order (disabled and unknown ones
/// left out; out-of-range page indexes go to the last page).
pub fn page_instances(list: &[WidgetInstance], n_pages: usize, page: usize) -> Vec<&WidgetInstance> {
    let last = n_pages.max(1) - 1;
    list.iter().filter(|w| w.enabled && w.kind != WidgetKind::Unknown && w.page.min(last) == page).collect()
}

/// Remove page `idx`: its widgets move to the previous page (or the next
/// one when it was the first); later pages shift down. The last page can't
/// be removed. Returns false if nothing changed.
pub fn remove_page(pages: &mut Vec<WidgetPage>, widgets: &mut [WidgetInstance], idx: usize) -> bool {
    if pages.len() <= 1 || idx >= pages.len() {
        return false;
    }
    pages.remove(idx);
    for w in widgets.iter_mut() {
        if w.page > idx || (w.page == idx && idx > 0) {
            w.page -= 1;
        }
    }
    true
}

/// Move page `from` to position `to`, keeping each widget on its page.
pub fn move_page(pages: &mut Vec<WidgetPage>, widgets: &mut [WidgetInstance], from: usize, to: usize) -> bool {
    if from >= pages.len() || to >= pages.len() || from == to {
        return false;
    }
    let p = pages.remove(from);
    pages.insert(to, p);
    for w in widgets.iter_mut() {
        let k = w.page.min(pages.len() - 1);
        w.page = if k == from {
            to
        } else if from < to && k > from && k <= to {
            k - 1
        } else if to < from && k >= to && k < from {
            k + 1
        } else {
            k
        };
    }
    true
}

// ----- export / import -------------------------------------------------------

pub const LAYOUT_FORMAT: &str = "powermac.notch.layout";
pub const LAYOUT_VERSION: u32 = 1;

/// A shareable Home layout: pages and widgets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutFile {
    pub format: String,
    pub version: u32,
    pub pages: Vec<WidgetPage>,
    pub widgets: Vec<WidgetInstance>,
}

/// Export pages + widgets as pretty JSON. Secret settings (API tokens) are
/// left out unless `include_secrets`.
pub fn export_layout(pages: &[WidgetPage], widgets: &[WidgetInstance], include_secrets: bool) -> String {
    let widgets = widgets
        .iter()
        .map(|w| {
            let mut w = w.clone();
            if !include_secrets {
                super::catalog::strip_secrets(w.kind, &mut w.settings);
            }
            w
        })
        .collect();
    let f = LayoutFile { format: LAYOUT_FORMAT.into(), version: LAYOUT_VERSION, pages: pages.to_vec(), widgets };
    serde_json::to_string_pretty(&f).unwrap_or_default()
}

/// How [`import_layout`] combines the file with the current layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMode {
    /// The file's pages and widgets replace the current ones.
    Replace,
    /// The file's pages are added after the current ones.
    Append,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImportResult {
    pub pages: Vec<WidgetPage>,
    pub widgets: Vec<WidgetInstance>,
    /// Problems that were fixed up (unknown kinds dropped, settings reset).
    pub warnings: Vec<String>,
}

/// Read a layout exported by [`export_layout`] (or a bare `widgets` array).
/// Settings are validated and normalized; imported instances get fresh ids
/// when they collide with existing ones.
pub fn import_layout(json_text: &str, current_pages: &[WidgetPage], current: &[WidgetInstance], mode: ImportMode) -> Result<ImportResult, String> {
    let v: Value = serde_json::from_str(json_text).map_err(|e| format!("Not valid JSON: {e}"))?;
    let (pages, list) = match &v {
        Value::Object(o) => {
            if let Some(f) = o.get("format").and_then(|f| f.as_str()) {
                if f != LAYOUT_FORMAT {
                    return Err(format!("Not a PowerMac notch layout (format \"{f}\")"));
                }
            }
            if o.get("version").and_then(|v| v.as_u64()).is_some_and(|n| n > LAYOUT_VERSION as u64) {
                return Err("The layout was made by a newer version of PowerMac".into());
            }
            let pages: Vec<WidgetPage> = o.get("pages").and_then(|p| serde_json::from_value(p.clone()).ok()).unwrap_or_default();
            let w = o.get("widgets").cloned().ok_or("The file has no \"widgets\"")?;
            (pages, w)
        }
        Value::Array(_) => (vec![], v.clone()),
        _ => return Err("Expected a layout object".into()),
    };
    let mut warnings = vec![];
    let parsed = match parse_widget_list(&list) {
        WidgetList::Instances(v) => v,
        WidgetList::Legacy(l) => migrate_legacy(&l, &LegacySettings::default()),
    };
    let n_file = list.as_array().map(|a| a.len()).unwrap_or(0);
    if parsed.len() < n_file {
        warnings.push(format!("{} malformed widget(s) skipped", n_file - parsed.len()));
    }
    let pages = if pages.is_empty() { WidgetPage::defaults() } else { pages };
    let (base_pages, mut out, offset) = match mode {
        ImportMode::Replace => (vec![], vec![], 0),
        ImportMode::Append => (current_pages.to_vec(), current.to_vec(), current_pages.len().max(1)),
    };
    let mut all_pages = if mode == ImportMode::Append && base_pages.is_empty() { WidgetPage::defaults() } else { base_pages };
    all_pages.extend(pages.iter().cloned());
    for mut w in parsed {
        if w.kind == WidgetKind::Unknown {
            warnings.push("A widget of an unknown kind was skipped".into());
            continue;
        }
        match super::catalog::validate_settings(w.kind, &w.settings) {
            Ok(n) => w.settings = n,
            Err(errs) => {
                warnings.push(format!("{}: {}", super::catalog::kind_info(w.kind).title, errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; ")));
                w.settings = super::catalog::normalize_settings(w.kind, &w.settings);
            }
        }
        w.page = w.page.min(pages.len() - 1) + offset;
        w.size = w.effective_size();
        if w.id.is_empty() || out.iter().any(|o: &WidgetInstance| o.id == w.id) {
            w.id = new_id();
        }
        out.push(w);
    }
    Ok(ImportResult { pages: all_pages, widgets: out, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::TempUnit;

    #[test]
    fn ids_are_uuids_and_unique() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4');
        assert!(matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
        assert_eq!(legacy_id(WidgetKind::Timer), legacy_id(WidgetKind::Timer));
        assert_ne!(legacy_id(WidgetKind::Timer), legacy_id(WidgetKind::Note));
        let all: std::collections::HashSet<String> = WidgetKind::ALL.iter().map(|k| legacy_id(*k)).collect();
        assert_eq!(all.len(), WidgetKind::ALL.len());
        assert_eq!(legacy_id(WidgetKind::Note).as_bytes()[14], b'4');
    }

    #[test]
    fn legacy_list_migrates_losslessly() {
        let old: Vec<WidgetConfig> = serde_json::from_str(
            r#"[{"kind":"weather","enabled":true},{"kind":"teleporter","enabled":true},{"kind":"timer","enabled":false},
                {"kind":"calendar"},{"kind":"weather","enabled":false},{"kind":"nowPlaying","enabled":true}]"#,
        )
        .unwrap();
        let legacy = LegacySettings {
            weather: WeatherConfig { location: "Berlin".into(), units: TempUnit::Fahrenheit },
            calendar: CalendarConfig { days: 3, calendars: vec!["Work".into()] },
            now_playing: NowPlayingConfig { use_applescript: true },
        };
        let m = migrate_legacy(&old, &legacy);
        let kinds: Vec<(WidgetKind, bool)> = m.iter().map(|w| (w.kind, w.enabled)).collect();
        assert_eq!(kinds, vec![(WidgetKind::Weather, true), (WidgetKind::Timer, false), (WidgetKind::Calendar, true), (WidgetKind::NowPlaying, true)]);
        assert!(m.iter().all(|w| w.page == 0));
        assert_eq!(m[0].settings["location"], "Berlin");
        assert_eq!(m[0].settings["units"], "fahrenheit");
        assert_eq!(m[0].size, WidgetSize::Small);
        assert_eq!(m[2].settings["days"], 3);
        assert_eq!(m[2].settings["calendars"], json!(["Work"]));
        assert_eq!(m[3].settings["useApplescript"], true);
        // Stable: migrating again gives the same ids.
        assert_eq!(migrate_legacy(&old, &legacy), m);
        // Every migrated setting validates.
        for w in &m {
            crate::widgets::catalog::validate_settings(w.kind, &w.settings).unwrap();
        }
    }

    #[test]
    fn widget_list_formats() {
        let v = json!([{"kind":"timer","enabled":false},{"kind":"note"}]);
        match parse_widget_list(&v) {
            WidgetList::Legacy(l) => assert_eq!(l.len(), 2),
            other => panic!("{other:?}"),
        }
        let v = json!([
            {"id":"a","kind":"script","size":"large","page":2,"settings":{"command":"date"}},
            {"id":"b","kind":"worldClock"},
            {"id":"a","kind":"note"},
            {"kind":"timer","enabled":true},
            {"id":"c","kind":"holodeck","settings":{"x":1}},
            42
        ]);
        let WidgetList::Instances(l) = parse_widget_list(&v) else { panic!() };
        let ids: Vec<&str> = l.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", &legacy_id(WidgetKind::Timer), "c"]);
        assert_eq!(l[0].size, WidgetSize::Large);
        assert_eq!(l[0].page, 2);
        assert_eq!(l[1].size, WidgetSize::Medium);
        assert!(l[1].enabled);
        assert_eq!(l[1].settings, json!({}));
        assert_eq!(l[3].kind, WidgetKind::Unknown);
        // Round trip.
        let back: Vec<WidgetInstance> = serde_json::from_value(serde_json::to_value(&l).unwrap()).unwrap();
        assert_eq!(back, l);
        assert!(matches!(parse_widget_list(&json!([])), WidgetList::Instances(v) if v.is_empty()));
        assert!(matches!(parse_widget_list(&json!("x")), WidgetList::Instances(v) if v.is_empty()));
    }

    #[test]
    fn sizes_snap_to_allowed() {
        let w = WidgetInstance::new(WidgetKind::Battery).sized(WidgetSize::Large);
        assert_eq!(w.effective_size(), WidgetSize::Medium);
        let w = WidgetInstance::new(WidgetKind::Script).sized(WidgetSize::Large);
        assert_eq!(w.effective_size(), WidgetSize::Large);
        assert_eq!(WidgetSize::Medium.width(), 2.0 * WidgetSize::Small.width() + CARD_GAP);
    }

    #[test]
    fn pages_filter_remove_and_move() {
        let mut pages = vec![WidgetPage::named("Work"), WidgetPage::named("Media"), WidgetPage::named("Dev")];
        let mut ws: Vec<WidgetInstance> = (0..5).map(|i| WidgetInstance::new(WidgetKind::Note).on_page([0, 1, 1, 2, 7][i])).collect();
        ws[1].enabled = false;
        assert_eq!(page_instances(&ws, 3, 1).len(), 1);
        assert_eq!(page_instances(&ws, 3, 2).len(), 2, "out of range goes to the last page");
        assert!(move_page(&mut pages, &mut ws, 2, 0));
        assert_eq!(pages[0].name, "Dev");
        assert_eq!(ws.iter().map(|w| w.page).collect::<Vec<_>>(), vec![1, 2, 2, 0, 0]);
        assert!(remove_page(&mut pages, &mut ws, 0));
        assert_eq!(pages.len(), 2);
        assert_eq!(ws.iter().map(|w| w.page).collect::<Vec<_>>(), vec![0, 1, 1, 0, 0]);
        assert!(remove_page(&mut pages, &mut ws, 1));
        assert!(ws.iter().all(|w| w.page == 0));
        assert!(!remove_page(&mut pages, &mut ws, 0));
    }

    #[test]
    fn export_import_round_trip() {
        let pages = vec![WidgetPage::named("Work"), WidgetPage::named("Dev")];
        let web = WidgetInstance::new(WidgetKind::Web).on_page(1).with_settings(json!({"url": "https://api.example.com/x", "token": "s3cret", "template": "{{price}}"}));
        let ws = vec![WidgetInstance::new(WidgetKind::Timer), web.clone()];
        let text = export_layout(&pages, &ws, false);
        assert!(!text.contains("s3cret"));
        assert!(export_layout(&pages, &ws, true).contains("s3cret"));
        let r = import_layout(&text, &[], &[], ImportMode::Replace).unwrap();
        assert_eq!(r.pages, pages);
        assert_eq!(r.widgets.len(), 2);
        assert_eq!(r.widgets[1].page, 1);
        assert_eq!(r.widgets[1].settings["url"], "https://api.example.com/x");
        assert_eq!(r.widgets[1].settings["token"], "");
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        // Append: pages after the current ones, colliding ids replaced.
        let r2 = import_layout(&text, &pages, &r.widgets, ImportMode::Append).unwrap();
        assert_eq!(r2.pages.len(), 4);
        assert_eq!(r2.widgets.len(), 4);
        assert_eq!(r2.widgets[3].page, 3);
        let ids: std::collections::HashSet<&str> = r2.widgets.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids.len(), 4);
        // Errors and fix-ups.
        assert!(import_layout("nope", &[], &[], ImportMode::Replace).is_err());
        assert!(import_layout(r#"{"format":"other","widgets":[]}"#, &[], &[], ImportMode::Replace).is_err());
        assert!(import_layout(r#"{"format":"powermac.notch.layout","version":99,"widgets":[]}"#, &[], &[], ImportMode::Replace).is_err());
        let r3 = import_layout(r#"[{"id":"x","kind":"worldClock","size":"large","page":5,"settings":{"style":"sundial"}},{"id":"y","kind":"unicorn"}]"#, &[], &[], ImportMode::Replace).unwrap();
        assert_eq!(r3.widgets.len(), 1);
        assert_eq!(r3.widgets[0].page, 0);
        assert_eq!(r3.widgets[0].settings["style"], "digital");
        assert_eq!(r3.warnings.len(), 2, "{:?}", r3.warnings);
    }
}
