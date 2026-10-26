//! The widget catalog: what each widget kind is, which sizes it allows and
//! a small settings schema the settings window renders as a generic form.
//!
//! Settings are stored per instance as a JSON object
//! ([`super::instance::WidgetInstance::settings`]). [`normalize_settings`]
//! fills in defaults and repairs wrong types / out-of-range values (what
//! the widgets use at runtime); [`validate_settings`] reports problems for
//! the settings UI and returns the normalized object when there are none.
//! The `*Settings` structs are the typed view the widgets read
//! ([`typed`]).

use std::sync::OnceLock;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::instance::WidgetSize;
use super::{TempUnit, WidgetKind};

// ----- schema --------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldType {
    /// One line of text (`multiline` for a text area).
    Text,
    /// A number (`min` / `max` / `step` / `unit`).
    Number,
    Bool,
    /// One of `choices` (stored as the choice's `value`).
    Choice,
    /// A list: of `item_type` values, or of objects with `item_fields`.
    List,
    /// Text that must not be shown or exported (API tokens).
    Secret,
    /// A file or folder path (`~` allowed).
    Path,
    /// An http(s) URL.
    Url,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
}

/// Show a field only while another field of the same object has one of
/// these values (e.g. the click target only when a click action is set).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowIf {
    pub field: &'static str,
    pub values: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingField {
    /// Key in the settings object (camelCase).
    pub name: &'static str,
    pub label: &'static str,
    #[serde(rename = "type")]
    pub ty: FieldType,
    pub default: Value,
    /// One-line explanation under the control.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub help: &'static str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub placeholder: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Choice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// Unit suffix for numbers ("s", "%").
    #[serde(skip_serializing_if = "str::is_empty")]
    pub unit: &'static str,
    pub required: bool,
    pub multiline: bool,
    /// Element type of a `List` (when `item_fields` is empty).
    pub item_type: FieldType,
    /// Fields of each object in a `List` of objects.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub item_fields: Vec<SettingField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_if: Option<ShowIf>,
}

impl SettingField {
    fn new(name: &'static str, label: &'static str, ty: FieldType, default: Value) -> Self {
        SettingField {
            name,
            label,
            ty,
            default,
            help: "",
            placeholder: "",
            choices: vec![],
            min: None,
            max: None,
            step: None,
            unit: "",
            required: false,
            multiline: false,
            item_type: FieldType::Text,
            item_fields: vec![],
            show_if: None,
        }
    }
    fn help(mut self, h: &'static str) -> Self {
        self.help = h;
        self
    }
    fn placeholder(mut self, p: &'static str) -> Self {
        self.placeholder = p;
        self
    }
    fn required(mut self) -> Self {
        self.required = true;
        self
    }
    fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }
    fn unit(mut self, u: &'static str) -> Self {
        self.unit = u;
        self
    }
    fn step(mut self, s: f64) -> Self {
        self.step = Some(s);
        self
    }
    fn of(mut self, t: FieldType) -> Self {
        self.item_type = t;
        self
    }
    fn fields(mut self, f: Vec<SettingField>) -> Self {
        self.item_fields = f;
        self
    }
    fn show_if(mut self, field: &'static str, values: &[&str]) -> Self {
        self.show_if = Some(ShowIf { field, values: values.iter().map(|v| json!(v)).collect() });
        self
    }
}

fn text(name: &'static str, label: &'static str, default: &str) -> SettingField {
    SettingField::new(name, label, FieldType::Text, json!(default))
}
fn secret(name: &'static str, label: &'static str) -> SettingField {
    SettingField::new(name, label, FieldType::Secret, json!(""))
}
fn path(name: &'static str, label: &'static str, default: &str) -> SettingField {
    SettingField::new(name, label, FieldType::Path, json!(default))
}
fn url(name: &'static str, label: &'static str, default: &str) -> SettingField {
    SettingField::new(name, label, FieldType::Url, json!(default))
}
fn boolean(name: &'static str, label: &'static str, default: bool) -> SettingField {
    SettingField::new(name, label, FieldType::Bool, json!(default))
}
fn number(name: &'static str, label: &'static str, default: f64, min: f64, max: f64) -> SettingField {
    let mut f = SettingField::new(name, label, FieldType::Number, num_value(default));
    f.min = Some(min);
    f.max = Some(max);
    f
}
fn choice(name: &'static str, label: &'static str, default: &str, choices: &[(&'static str, &'static str)]) -> SettingField {
    let mut f = SettingField::new(name, label, FieldType::Choice, json!(default));
    f.choices = choices.iter().map(|(value, label)| Choice { value, label }).collect();
    f
}
fn list(name: &'static str, label: &'static str, default: Value) -> SettingField {
    SettingField::new(name, label, FieldType::List, default)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    /// Script, xbar plugins, Web / JSON, Shortcuts, Launcher.
    BuildYourOwn,
    Information,
    Media,
    System,
    Tools,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetKindInfo {
    pub kind: WidgetKind,
    /// The kind's id as stored in configs ("worldClock").
    pub id: &'static str,
    pub title: &'static str,
    pub sf_symbol: &'static str,
    pub description: &'static str,
    pub category: Category,
    pub default_size: WidgetSize,
    /// Allowed sizes, smallest first.
    pub sizes: Vec<WidgetSize>,
    pub fields: Vec<SettingField>,
    /// What enabling it involves (permissions, network), if anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_note: Option<&'static str>,
    /// Works on macOS only (the whole Home view is macOS-only today).
    pub macos_only: bool,
}

const ALL_SIZES: [WidgetSize; 3] = [WidgetSize::Small, WidgetSize::Medium, WidgetSize::Large];

fn sizes(s: &[WidgetSize]) -> Vec<WidgetSize> {
    s.to_vec()
}

/// Default plugin folder for the xbar widget (shown as `~/...`).
pub const DEFAULT_PLUGIN_DIR: &str = "~/Library/Application Support/PowerMac/plugins";

fn build_info(kind: WidgetKind) -> WidgetKindInfo {
    use WidgetSize::{Large as L, Medium as M, Small as S};
    let info = |id, title, sf_symbol, description, category, default_size, allowed: &[WidgetSize], fields: Vec<SettingField>| WidgetKindInfo {
        kind,
        id,
        title,
        sf_symbol,
        description,
        category,
        default_size,
        sizes: sizes(allowed),
        fields,
        permission_note: kind.note(),
        macos_only: true,
    };
    let interval = |default: f64, min: f64| number("interval", "Refresh every", default, min, 86_400.0).unit("s").step(1.0);
    let live = boolean("live", "Live activity", false).help("Show the value beside the closed notch and keep refreshing while it's closed.");
    match kind {
        WidgetKind::NowPlaying => info("nowPlaying", "Now Playing", "music.note", "The current track with artwork, progress and playback controls.", Category::Media, M, &[M, L], vec![
            boolean("useApplescript", "Read Music and Spotify directly", false).help("When the system's Now Playing information is empty (macOS 15.4+), ask Music and Spotify with AppleScript. macOS asks once per app for Automation access."),
        ]),
        WidgetKind::Calendar => info("calendar", "Calendar", "calendar", "Your next events from the Calendar app.", Category::Information, M, &[M, L], vec![
            number("days", "Days ahead", 2.0, 1.0, 7.0).step(1.0).unit("days").help("Starting today."),
            list("calendars", "Calendars", json!([])).help("Calendar names to show; empty shows all calendars."),
        ]),
        WidgetKind::Weather => info("weather", "Weather", "cloud.sun.fill", "Current conditions and today's high and low from Open-Meteo.", Category::Information, S, &[S, M], vec![
            text("location", "Location", "").placeholder("Berlin").help("A city (\u{201c}Portland, Oregon\u{201d}) or \u{201c}latitude, longitude\u{201d}."),
            choice("units", "Temperature", "auto", &[("auto", "Automatic"), ("celsius", "°C"), ("fahrenheit", "°F")]),
        ]),
        WidgetKind::Battery => info("battery", "Battery", "battery.75percent", "This Mac's battery level and time remaining.", Category::System, S, &[S, M], vec![]),
        WidgetKind::SystemStats => info("systemStats", "System Stats", "cpu", "CPU, memory and network activity with sparklines.", Category::System, M, &[M, L], vec![]),
        WidgetKind::Timer => info("timer", "Timer & Stopwatch", "timer", "Countdown presets and a stopwatch; shows beside the closed notch while running.", Category::Tools, M, &[M, L], vec![
            list("presets", "Presets", json!([5, 10, 25])).of(FieldType::Number).help("Countdown buttons, in minutes (up to 5)."),
        ]),
        WidgetKind::QuickToggles => info("quickToggles", "Quick Controls", "switch.2", "Buttons for PowerMac's toggles and actions (Awake, Color Picker, ...).", Category::Tools, M, &ALL_SIZES, vec![
            list("controls", "Controls", json!([])).help("Ids of the controls to show, in order; empty shows all."),
        ]),
        WidgetKind::Note => info("note", "Note", "note.text", "A scratchpad; click to type. Each note widget keeps its own text.", Category::Tools, M, &ALL_SIZES, vec![]),
        WidgetKind::Mirror => info("mirror", "Mirror", "person.crop.square", "A quick look through the camera.", Category::Tools, S, &[S, M], vec![]),
        WidgetKind::Script => info("script", "Script", "terminal", "Runs your command every few seconds and shows its output as text, a number with a gauge, a list or JSON.", Category::BuildYourOwn, M, &ALL_SIZES, vec![
            text("command", "Command", "").required().multiline().placeholder("df -h / | awk 'NR==2 {print $5}'").help("Runs with the interpreter below. Only commands you enter here are ever run."),
            choice("interpreter", "Interpreter", "zsh", &[("zsh", "zsh (login shell)"), ("bash", "bash (login shell)"), ("sh", "sh"), ("python3", "Python 3"), ("node", "Node.js"), ("ruby", "Ruby"), ("perl", "Perl"), ("osascript", "AppleScript"), ("custom", "Custom…")]),
            path("interpreterPath", "Interpreter path", "").placeholder("/opt/homebrew/bin/fish").help("Called as <path> -c <command>.").show_if("interpreter", &["custom"]),
            interval(60.0, 1.0),
            number("timeout", "Time limit", 10.0, 1.0, 600.0).unit("s").step(1.0).help("The command is stopped after this long."),
            choice("format", "Output", "auto", &[("auto", "Automatic"), ("text", "Text (first line is the title)"), ("number", "Number"), ("list", "List (one item per line)"), ("json", "JSON")]).help("JSON: {title, subtitle, value, progress, color, sf_symbol, items: [{title, subtitle, action}]}."),
            text("title", "Title", "").placeholder("Disk").help("Card title; empty uses the output's title."),
            text("symbol", "Symbol", "terminal").placeholder("internaldrive").help("SF Symbol name."),
            text("unit", "Unit", "").placeholder("%").help("For numbers without a unit of their own."),
            boolean("gauge", "Show a gauge", true).help("For numbers: a ring from Minimum to Maximum (percentages use 0–100)."),
            number("min", "Minimum", 0.0, -1e12, 1e12).show_if("gauge", &["true"]),
            number("max", "Maximum", 100.0, -1e12, 1e12).show_if("gauge", &["true"]),
            choice("click", "When clicked", "none", &[("none", "Nothing"), ("refresh", "Refresh"), ("openUrl", "Open a URL"), ("run", "Run a command"), ("copy", "Copy the output")]),
            text("clickTarget", "Click target", "").placeholder("https://… or a command").show_if("click", &["openUrl", "run"]),
            path("workingDir", "Working folder", "~").help("Where the command runs."),
            live,
        ]),
        WidgetKind::Xbar => info("xbar", "xbar Plugins", "puzzlepiece.extension", "Runs xbar / SwiftBar / BitBar plugins and shows their title and menu items; click an item to run its action.", Category::BuildYourOwn, M, &ALL_SIZES, vec![
            path("path", "Plugin or folder", DEFAULT_PLUGIN_DIR).required().help("A plugin file (name.5m.sh) or a folder of plugins. Plugins are only ever run from here."),
            number("interval", "Refresh every", 0.0, 0.0, 86_400.0).unit("s").step(1.0).help("0 uses the interval in the file name (name.10s.sh, .5m., .1h., .1d.)."),
            text("title", "Title", "").help("Card title; empty uses the plugin's name."),
            number("timeout", "Time limit", 30.0, 1.0, 600.0).unit("s").step(1.0),
            live,
        ]),
        WidgetKind::Web => info("web", "Web / JSON", "network", "Fetches a URL and shows values picked from the JSON response with a template.", Category::BuildYourOwn, M, &ALL_SIZES, vec![
            url("url", "URL", "").required().placeholder("https://api.example.com/v1/price"),
            list("headers", "Headers", json!([])).help("\u{201c}Name: value\u{201d}, one per entry."),
            secret("token", "API token").help("Sent as the header below. Stored in PowerMac's settings, never exported."),
            text("tokenHeader", "Token header", "Authorization"),
            text("tokenPrefix", "Token prefix", "Bearer ").help("Put before the token (\u{201c}Bearer \u{201d}, \u{201c}token \u{201d}, or empty)."),
            text("title", "Title", "").placeholder("Bitcoin"),
            text("template", "Value", "").required().placeholder("{{data.price | round:2}} {{data.currency}}").help("Text with {{path}} placeholders; filters: round:N, upper, lower, default:X, join:SEP, len, percent, bytes, int, date."),
            text("subtitle", "Subtitle", "").placeholder("Updated {{data.time}}"),
            text("valuePath", "Gauge value", "").placeholder("data.percent").help("A path to a number for the gauge; empty for no gauge."),
            number("min", "Gauge minimum", 0.0, -1e12, 1e12),
            number("max", "Gauge maximum", 100.0, -1e12, 1e12),
            text("listPath", "List", "").placeholder("data.items").help("A path to an array to show as a list."),
            text("itemTemplate", "List item", "{{title}}").placeholder("{{name}} — {{value}}"),
            text("symbol", "Symbol", "network").help("SF Symbol name."),
            text("openUrl", "Open when clicked", "").placeholder("https://example.com/{{data.id}}"),
            interval(300.0, 10.0),
            number("timeout", "Time limit", 15.0, 1.0, 120.0).unit("s").step(1.0),
            boolean("allowHttp", "Allow plain HTTP", false).help("Only HTTPS URLs are fetched unless this is on."),
            live,
        ]),
        WidgetKind::Shortcuts => info("shortcuts", "Shortcuts", "square.2.layers.3d", "Buttons that run your Apple Shortcuts, optionally showing their text output.", Category::BuildYourOwn, M, &ALL_SIZES, vec![
            list("shortcuts", "Shortcuts", json!([])).required().help("Shortcut names, as in the Shortcuts app."),
            boolean("showOutput", "Show output", true).help("Show the text a shortcut returns."),
            text("title", "Title", "Shortcuts"),
        ]),
        WidgetKind::Launcher => info("launcher", "Launcher", "square.grid.2x2", "A grid of apps, files, folders, links and commands.", Category::BuildYourOwn, M, &ALL_SIZES, vec![
            list("items", "Items", json!([])).required().fields(vec![
                choice("kind", "Kind", "app", &[("app", "App"), ("file", "File"), ("folder", "Folder"), ("url", "Link"), ("command", "Command")]),
                text("target", "Target", "").required().placeholder("Safari, ~/Documents, https://…, or a command").help("An app name, bundle id or path; a path; a URL; or a zsh command."),
                text("label", "Label", "").help("Empty uses the app or file name."),
                text("symbol", "Symbol", "").help("SF Symbol for links and commands."),
            ]),
            boolean("labels", "Show labels", true),
            text("title", "Title", ""),
        ]),
        WidgetKind::Bluetooth => info("bluetooth", "Bluetooth Batteries", "airpodspro", "Battery levels of AirPods (left, right, case), Magic Mouse, Keyboard, Trackpad and other connected devices.", Category::System, M, &ALL_SIZES, vec![
            list("devices", "Devices", json!([])).help("Device names to show; empty shows every connected device that reports a battery."),
        ]),
        WidgetKind::Sound => info("sound", "Sound", "speaker.wave.2.fill", "Output device switcher, volume slider and mute.", Category::Media, M, &[M, L], vec![
            boolean("showDevices", "Show output devices", true),
        ]),
        WidgetKind::Reminders => info("reminders", "Reminders", "checklist", "Incomplete reminders from the Reminders app; tick one to complete it.", Category::Information, M, &[M, L], vec![
            list("lists", "Lists", json!([])).help("Reminder list names; empty shows all lists."),
            choice("show", "Show", "all", &[("all", "All incomplete"), ("today", "Due today or overdue"), ("scheduled", "With a due date")]),
        ]),
        WidgetKind::WorldClock => info("worldClock", "World Clocks", "globe", "The time in other time zones, analog or digital, with a day / night indicator.", Category::Information, M, &ALL_SIZES, vec![
            list("clocks", "Clocks", json!([{"zone": "America/Los_Angeles", "label": "Cupertino"}, {"zone": "Europe/London", "label": "London"}, {"zone": "Asia/Tokyo", "label": "Tokyo"}])).fields(vec![
                text("zone", "Time zone", "").required().placeholder("Europe/Paris").help("An IANA time zone name."),
                text("label", "Label", "").placeholder("Paris"),
            ]),
            choice("style", "Style", "digital", &[("digital", "Digital"), ("analog", "Analog")]),
            choice("hours", "Hours", "auto", &[("auto", "System setting"), ("12", "12-hour"), ("24", "24-hour")]),
            boolean("seconds", "Show seconds", false),
        ]),
        WidgetKind::Unknown => info("unknown", "Unknown", "questionmark", "A widget from a newer version of PowerMac.", Category::Tools, M, &[M], vec![]),
    }
}

/// Every widget kind that can be added, in catalog order.
pub fn widget_catalog() -> Vec<WidgetKindInfo> {
    all_infos().iter().filter(|i| i.kind != WidgetKind::Unknown).cloned().collect()
}

fn all_infos() -> &'static Vec<WidgetKindInfo> {
    static C: OnceLock<Vec<WidgetKindInfo>> = OnceLock::new();
    C.get_or_init(|| WidgetKind::ALL.iter().copied().chain([WidgetKind::Unknown]).map(build_info).collect())
}

pub fn kind_info(kind: WidgetKind) -> &'static WidgetKindInfo {
    all_infos().iter().find(|i| i.kind == kind).unwrap_or_else(|| all_infos().last().unwrap())
}

/// The kind with this config id ("worldClock").
pub fn kind_by_id(id: &str) -> Option<WidgetKind> {
    all_infos().iter().find(|i| i.id == id && i.kind != WidgetKind::Unknown).map(|i| i.kind)
}

/// All defaults of a kind as a settings object.
pub fn default_settings(kind: WidgetKind) -> Value {
    Value::Object(kind_info(kind).fields.iter().map(|f| (f.name.to_string(), f.default.clone())).collect())
}

// ----- normalization / validation ---------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldError {
    /// Field name (`clocks[1].zone` for list items, empty for the object).
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.field.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.field, self.message)
        }
    }
}

fn coerce_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
}

fn coerce_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        },
        Value::Number(n) => n.as_f64().map(|n| n != 0.0),
        _ => None,
    }
}

fn coerce_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Normalize one value of field `f`; errors go to `errs` (prefixed `at`).
fn norm_value(f: &SettingField, v: Option<&Value>, at: &str, errs: &mut Vec<FieldError>) -> Value {
    let err = |errs: &mut Vec<FieldError>, m: String| errs.push(FieldError { field: at.to_string(), message: m });
    let Some(v) = v.filter(|v| !v.is_null()) else {
        if f.required && is_blank(&f.default) {
            err(errs, "is required".into());
        }
        return f.default.clone();
    };
    match f.ty {
        FieldType::Number => match coerce_number(v) {
            Some(mut n) => {
                if let Some(min) = f.min.filter(|m| n < *m) {
                    err(errs, format!("must be at least {}{}", fmt_num(min), f.unit));
                    n = min;
                }
                if let Some(max) = f.max.filter(|m| n > *m) {
                    err(errs, format!("must be at most {}{}", fmt_num(max), f.unit));
                    n = max;
                }
                num_value(n)
            }
            None => {
                err(errs, "must be a number".into());
                f.default.clone()
            }
        },
        FieldType::Bool => match coerce_bool(v) {
            Some(b) => json!(b),
            None => {
                err(errs, "must be on or off".into());
                f.default.clone()
            }
        },
        FieldType::Choice => match coerce_text(v) {
            Some(s) if f.choices.iter().any(|c| c.value == s) => json!(s),
            _ => {
                err(errs, format!("must be one of {}", f.choices.iter().map(|c| c.value).collect::<Vec<_>>().join(", ")));
                f.default.clone()
            }
        },
        FieldType::Text | FieldType::Secret | FieldType::Path | FieldType::Url => match coerce_text(v) {
            Some(s) => {
                if f.required && s.trim().is_empty() {
                    err(errs, "is required".into());
                }
                json!(s)
            }
            None => {
                err(errs, "must be text".into());
                f.default.clone()
            }
        },
        FieldType::List => {
            let items: Vec<Value> = match v {
                Value::Array(a) => a.clone(),
                // A text area with one entry per line.
                Value::String(s) if f.item_fields.is_empty() => s.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| json!(l)).collect(),
                _ => {
                    err(errs, "must be a list".into());
                    return f.default.clone();
                }
            };
            let mut out = vec![];
            for (i, it) in items.iter().enumerate() {
                let here = format!("{at}[{i}]");
                if !f.item_fields.is_empty() {
                    match it {
                        Value::Object(o) => out.push(Value::Object(norm_object(&f.item_fields, o, &here, errs))),
                        _ => err(errs, format!("item {} must be an object", i + 1)),
                    }
                } else {
                    match f.item_type {
                        FieldType::Number => match coerce_number(it) {
                            Some(n) => out.push(num_value(n)),
                            None => err(errs, format!("item {} must be a number", i + 1)),
                        },
                        _ => match coerce_text(it) {
                            Some(s) if !s.trim().is_empty() => out.push(json!(s.trim())),
                            Some(_) => {}
                            None => err(errs, format!("item {} must be text", i + 1)),
                        },
                    }
                }
            }
            if f.required && out.is_empty() {
                err(errs, "needs at least one entry".into());
            }
            Value::Array(out)
        }
    }
}

/// Integral numbers stay integers in JSON (2, not 2.0).
fn num_value(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9e15 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

fn is_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn norm_object(fields: &[SettingField], o: &Map<String, Value>, at: &str, errs: &mut Vec<FieldError>) -> Map<String, Value> {
    // Unknown keys are kept (settings written by a newer version).
    let mut out = o.clone();
    for f in fields {
        let here = if at.is_empty() { f.name.to_string() } else { format!("{at}.{}", f.name) };
        let n = norm_value(f, o.get(f.name), &here, errs);
        out.insert(f.name.to_string(), n);
    }
    out
}

/// Settings with defaults filled in and bad values replaced (never fails).
pub fn normalize_settings(kind: WidgetKind, v: &Value) -> Value {
    let empty = Map::new();
    let o = v.as_object().unwrap_or(&empty);
    let mut errs = vec![];
    Value::Object(norm_object(&kind_info(kind).fields, o, "", &mut errs))
}

/// Check settings for the settings UI: `Ok(normalized)` or every problem
/// found (types, ranges, choices, required fields, URLs, time zones,
/// templates and paths).
pub fn validate_settings(kind: WidgetKind, v: &Value) -> Result<Value, Vec<FieldError>> {
    let mut errs = vec![];
    let empty = Map::new();
    let o = match v {
        Value::Object(o) => o,
        Value::Null => &empty,
        _ => {
            return Err(vec![FieldError { field: String::new(), message: "settings must be an object".into() }]);
        }
    };
    let n = Value::Object(norm_object(&kind_info(kind).fields, o, "", &mut errs));
    check_kind(kind, &n, &mut errs);
    if errs.is_empty() {
        Ok(n)
    } else {
        Err(errs)
    }
}

fn check_kind(kind: WidgetKind, n: &Value, errs: &mut Vec<FieldError>) {
    let s = |k: &str| n.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut push = |field: &str, message: String| errs.push(FieldError { field: field.into(), message });
    match kind {
        WidgetKind::Web => {
            let u = s("url");
            let allow_http = n.get("allowHttp").and_then(|v| v.as_bool()).unwrap_or(false);
            if !u.trim().is_empty() {
                if let Err(e) = super::web::check_url(&u, allow_http) {
                    push("url", e);
                }
            }
            for (i, h) in n.get("headers").and_then(|h| h.as_array()).into_iter().flatten().enumerate() {
                if super::web::parse_header(h.as_str().unwrap_or("")).is_none() {
                    push(&format!("headers[{i}]"), "must look like \u{201c}Name: value\u{201d}".into());
                }
            }
            for k in ["template", "subtitle", "itemTemplate", "openUrl"] {
                if let Err(e) = super::web::Template::parse(&s(k)) {
                    push(k, e);
                }
            }
            for k in ["valuePath", "listPath"] {
                let p = s(k);
                if !p.trim().is_empty() {
                    if let Err(e) = super::web::JsonPath::parse(&p) {
                        push(k, e);
                    }
                }
            }
            let (min, max) = (n["min"].as_f64().unwrap_or(0.0), n["max"].as_f64().unwrap_or(100.0));
            if max <= min && !s("valuePath").trim().is_empty() {
                push("max", "must be greater than the minimum".into());
            }
        }
        WidgetKind::Script => {
            if s("interpreter") == "custom" && s("interpreterPath").trim().is_empty() {
                push("interpreterPath", "is required for a custom interpreter".into());
            }
            if matches!(s("click").as_str(), "openUrl" | "run") && s("clickTarget").trim().is_empty() {
                push("clickTarget", "is required for this click action".into());
            }
            let (min, max) = (n["min"].as_f64().unwrap_or(0.0), n["max"].as_f64().unwrap_or(100.0));
            if max <= min {
                push("max", "must be greater than the minimum".into());
            }
        }
        WidgetKind::WorldClock => {
            for (i, c) in n.get("clocks").and_then(|c| c.as_array()).into_iter().flatten().enumerate() {
                let z = c.get("zone").and_then(|z| z.as_str()).unwrap_or("");
                if !z.trim().is_empty() && !super::clocks::zone_valid(z) {
                    push(&format!("clocks[{i}].zone"), format!("\u{201c}{z}\u{201d} isn't a known time zone"));
                }
            }
        }
        WidgetKind::Launcher => {
            for (i, it) in n.get("items").and_then(|c| c.as_array()).into_iter().flatten().enumerate() {
                let kind = it.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                let target = it.get("target").and_then(|v| v.as_str()).unwrap_or("");
                if kind == "url" && !target.is_empty() && !target.contains(':') {
                    push(&format!("items[{i}].target"), "must be a URL (https://…)".into());
                }
            }
        }
        _ => {}
    }
}

/// Blank out secret fields (for exports).
pub fn strip_secrets(kind: WidgetKind, v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        for f in &kind_info(kind).fields {
            if f.ty == FieldType::Secret && o.contains_key(f.name) {
                o.insert(f.name.to_string(), json!(""));
            }
        }
    }
}

/// The typed settings of a kind (normalized first, so this never fails).
pub fn typed<T: DeserializeOwned + Default>(kind: WidgetKind, v: &Value) -> T {
    serde_json::from_value(normalize_settings(kind, v)).unwrap_or_default()
}

// ----- typed settings ------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NowPlayingSettings {
    pub use_applescript: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarSettings {
    pub days: f64,
    pub calendars: Vec<String>,
}

impl Default for CalendarSettings {
    fn default() -> Self {
        CalendarSettings { days: 2.0, calendars: vec![] }
    }
}

#[derive(Clone, Debug, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WeatherSettings {
    pub location: String,
    pub units: TempUnit,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TimerSettings {
    /// Minutes.
    pub presets: Vec<f64>,
}

impl Default for TimerSettings {
    fn default() -> Self {
        TimerSettings { presets: vec![5.0, 10.0, 25.0] }
    }
}

impl TimerSettings {
    /// Preset seconds (1–5 presets, each 1 s .. 24 h).
    pub fn preset_secs(&self) -> Vec<u32> {
        let v: Vec<u32> = self.presets.iter().filter(|m| **m > 0.0).take(5).map(|m| (m * 60.0).clamp(1.0, 86_400.0) as u32).collect();
        if v.is_empty() {
            super::timer::PRESETS.to_vec()
        } else {
            v
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QuickControlsSettings {
    pub controls: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Interpreter {
    #[default]
    Zsh,
    Bash,
    Sh,
    Python3,
    Node,
    Ruby,
    Perl,
    Osascript,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputFormat {
    #[default]
    Auto,
    Text,
    Number,
    List,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ClickAction {
    #[default]
    None,
    Refresh,
    OpenUrl,
    Run,
    Copy,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScriptSettings {
    pub command: String,
    pub interpreter: Interpreter,
    pub interpreter_path: String,
    pub interval: f64,
    pub timeout: f64,
    pub format: OutputFormat,
    pub title: String,
    pub symbol: String,
    pub unit: String,
    pub gauge: bool,
    pub min: f64,
    pub max: f64,
    pub click: ClickAction,
    pub click_target: String,
    pub working_dir: String,
    pub live: bool,
}

impl Default for ScriptSettings {
    fn default() -> Self {
        ScriptSettings {
            command: String::new(),
            interpreter: Interpreter::Zsh,
            interpreter_path: String::new(),
            interval: 60.0,
            timeout: 10.0,
            format: OutputFormat::Auto,
            title: String::new(),
            symbol: "terminal".into(),
            unit: String::new(),
            gauge: true,
            min: 0.0,
            max: 100.0,
            click: ClickAction::None,
            click_target: String::new(),
            working_dir: "~".into(),
            live: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct XbarSettings {
    pub path: String,
    /// Seconds; 0 = from the file name.
    pub interval: f64,
    pub title: String,
    pub timeout: f64,
    pub live: bool,
}

impl Default for XbarSettings {
    fn default() -> Self {
        XbarSettings { path: DEFAULT_PLUGIN_DIR.into(), interval: 0.0, title: String::new(), timeout: 30.0, live: false }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WebSettings {
    pub url: String,
    pub headers: Vec<String>,
    pub token: String,
    pub token_header: String,
    pub token_prefix: String,
    pub title: String,
    pub template: String,
    pub subtitle: String,
    pub value_path: String,
    pub min: f64,
    pub max: f64,
    pub list_path: String,
    pub item_template: String,
    pub symbol: String,
    pub open_url: String,
    pub interval: f64,
    pub timeout: f64,
    pub allow_http: bool,
    pub live: bool,
}

impl Default for WebSettings {
    fn default() -> Self {
        WebSettings {
            url: String::new(),
            headers: vec![],
            token: String::new(),
            token_header: "Authorization".into(),
            token_prefix: "Bearer ".into(),
            title: String::new(),
            template: String::new(),
            subtitle: String::new(),
            value_path: String::new(),
            min: 0.0,
            max: 100.0,
            list_path: String::new(),
            item_template: "{{title}}".into(),
            symbol: "network".into(),
            open_url: String::new(),
            interval: 300.0,
            timeout: 15.0,
            allow_http: false,
            live: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShortcutsSettings {
    pub shortcuts: Vec<String>,
    pub show_output: bool,
    pub title: String,
}

impl Default for ShortcutsSettings {
    fn default() -> Self {
        ShortcutsSettings { shortcuts: vec![], show_output: true, title: "Shortcuts".into() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize, Hash)]
#[serde(rename_all = "camelCase")]
pub enum LaunchKind {
    #[default]
    App,
    File,
    Folder,
    Url,
    Command,
}

#[derive(Clone, Debug, PartialEq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LaunchItem {
    pub kind: LaunchKind,
    pub target: String,
    pub label: String,
    pub symbol: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LauncherSettings {
    pub items: Vec<LaunchItem>,
    pub labels: bool,
    pub title: String,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        LauncherSettings { items: vec![], labels: true, title: String::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BluetoothSettings {
    pub devices: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SoundSettings {
    pub show_devices: bool,
}

impl Default for SoundSettings {
    fn default() -> Self {
        SoundSettings { show_devices: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReminderFilter {
    #[default]
    All,
    Today,
    Scheduled,
}

#[derive(Clone, Debug, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemindersSettings {
    pub lists: Vec<String>,
    pub show: ReminderFilter,
}

#[derive(Clone, Debug, PartialEq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClockEntry {
    pub zone: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ClockStyle {
    #[default]
    Digital,
    Analog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum HourFormat {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "12")]
    H12,
    #[serde(rename = "24")]
    H24,
}

#[derive(Clone, Debug, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorldClockSettings {
    pub clocks: Vec<ClockEntry>,
    pub style: ClockStyle,
    pub hours: HourFormat,
    pub seconds: bool,
}

/// The typed settings of an instance, by kind.
#[derive(Clone, Debug, PartialEq)]
pub enum KindSettings {
    NowPlaying(NowPlayingSettings),
    Calendar(CalendarSettings),
    Weather(WeatherSettings),
    Timer(TimerSettings),
    QuickToggles(QuickControlsSettings),
    Script(ScriptSettings),
    Xbar(XbarSettings),
    Web(WebSettings),
    Shortcuts(ShortcutsSettings),
    Launcher(LauncherSettings),
    Bluetooth(BluetoothSettings),
    Sound(SoundSettings),
    Reminders(RemindersSettings),
    WorldClock(WorldClockSettings),
    /// Kinds without settings.
    None,
}

impl KindSettings {
    pub fn parse(kind: WidgetKind, v: &Value) -> KindSettings {
        match kind {
            WidgetKind::NowPlaying => KindSettings::NowPlaying(typed(kind, v)),
            WidgetKind::Calendar => KindSettings::Calendar(typed(kind, v)),
            WidgetKind::Weather => KindSettings::Weather(typed(kind, v)),
            WidgetKind::Timer => KindSettings::Timer(typed(kind, v)),
            WidgetKind::QuickToggles => KindSettings::QuickToggles(typed(kind, v)),
            WidgetKind::Script => KindSettings::Script(typed(kind, v)),
            WidgetKind::Xbar => KindSettings::Xbar(typed(kind, v)),
            WidgetKind::Web => KindSettings::Web(typed(kind, v)),
            WidgetKind::Shortcuts => KindSettings::Shortcuts(typed(kind, v)),
            WidgetKind::Launcher => KindSettings::Launcher(typed(kind, v)),
            WidgetKind::Bluetooth => KindSettings::Bluetooth(typed(kind, v)),
            WidgetKind::Sound => KindSettings::Sound(typed(kind, v)),
            WidgetKind::Reminders => KindSettings::Reminders(typed(kind, v)),
            WidgetKind::WorldClock => KindSettings::WorldClock(typed(kind, v)),
            _ => KindSettings::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_complete_and_consistent() {
        let cat = widget_catalog();
        assert_eq!(cat.len(), WidgetKind::ALL.len());
        let ids: std::collections::HashSet<&str> = cat.iter().map(|i| i.id).collect();
        assert_eq!(ids.len(), cat.len());
        for i in &cat {
            // The id is the serde name of the kind.
            assert_eq!(serde_json::to_value(i.kind).unwrap(), json!(i.id));
            assert_eq!(kind_by_id(i.id), Some(i.kind));
            assert!(i.sizes.contains(&i.default_size), "{}", i.id);
            assert!(i.sizes.windows(2).all(|w| w[0] < w[1]), "{}", i.id);
            assert!(!i.title.is_empty() && !i.sf_symbol.is_empty() && !i.description.is_empty());
            // Defaults normalize to themselves and validate (except required fields).
            let d = default_settings(i.kind);
            assert_eq!(normalize_settings(i.kind, &d), d, "{}", i.id);
            let names: std::collections::HashSet<&str> = i.fields.iter().map(|f| f.name).collect();
            assert_eq!(names.len(), i.fields.len(), "{}: duplicate field", i.id);
            for f in &i.fields {
                if f.ty == FieldType::Choice {
                    assert!(f.choices.iter().any(|c| json!(c.value) == f.default), "{}.{}", i.id, f.name);
                }
                if let Some(s) = &f.show_if {
                    assert!(names.contains(s.field), "{}.{}", i.id, f.name);
                }
            }
            let required = i.fields.iter().any(|f| f.required && is_blank(&f.default));
            assert_eq!(validate_settings(i.kind, &d).is_ok(), !required, "{}", i.id);
        }
        // Serializable for the settings UI.
        let v = serde_json::to_value(&cat).unwrap();
        let script = v.as_array().unwrap().iter().find(|k| k["id"] == "script").unwrap();
        assert_eq!(script["fields"][0]["type"], "text");
        assert_eq!(script["fields"][0]["required"], true);
        assert_eq!(script["defaultSize"], "medium");
        assert_eq!(kind_by_id("unknown"), None);
    }

    #[test]
    fn normalization_repairs_values() {
        let v = json!({"command": "uptime", "interval": "0.2", "timeout": 5000, "format": "yaml", "gauge": "yes", "extra": 1});
        let n = normalize_settings(WidgetKind::Script, &v);
        assert_eq!(n["interval"], 1.0);
        assert_eq!(n["timeout"], 600.0);
        assert_eq!(n["format"], "auto");
        assert_eq!(n["gauge"], true);
        assert_eq!(n["extra"], 1, "unknown keys are kept");
        assert_eq!(n["symbol"], "terminal");
        let errs = validate_settings(WidgetKind::Script, &v).unwrap_err();
        let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
        assert_eq!(fields, vec!["interval", "timeout", "format"]);
        assert!(errs[0].to_string().contains("at least 1s"), "{}", errs[0]);
        let s: ScriptSettings = typed(WidgetKind::Script, &v);
        assert_eq!((s.command.as_str(), s.interval, s.timeout, s.format, s.gauge), ("uptime", 1.0, 600.0, OutputFormat::Auto, true));
        // Not an object.
        assert!(validate_settings(WidgetKind::Script, &json!([1])).is_err());
        assert_eq!(normalize_settings(WidgetKind::Battery, &json!("x")), json!({}));
    }

    #[test]
    fn lists_and_objects() {
        let v = json!({"items": [{"kind": "url", "target": "https://rust-lang.org"}, {"kind": "teleport", "target": "x"}, 7, {"target": ""}]});
        let errs = validate_settings(WidgetKind::Launcher, &v).unwrap_err();
        let msgs: Vec<String> = errs.iter().map(|e| e.to_string()).collect();
        assert!(msgs.iter().any(|m| m.starts_with("items[1].kind")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("item 3 must be an object")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.starts_with("items[3].target: is required")), "{msgs:?}");
        let l: LauncherSettings = typed(WidgetKind::Launcher, &v);
        assert_eq!(l.items.len(), 3);
        assert_eq!(l.items[0].kind, LaunchKind::Url);
        assert_eq!(l.items[1].kind, LaunchKind::App);
        assert!(l.labels);
        // Text lists accept a multi-line string.
        let s: ShortcutsSettings = typed(WidgetKind::Shortcuts, &json!({"shortcuts": "Focus\n\n  Log Water \n"}));
        assert_eq!(s.shortcuts, vec!["Focus", "Log Water"]);
        // Number lists.
        let t: TimerSettings = typed(WidgetKind::Timer, &json!({"presets": [1, "3", "x", 90]}));
        assert_eq!(t.presets, vec![1.0, 3.0, 90.0]);
        assert_eq!(t.preset_secs(), vec![60, 180, 5400]);
        assert_eq!(TimerSettings { presets: vec![] }.preset_secs(), super::super::timer::PRESETS.to_vec());
        assert!(validate_settings(WidgetKind::Shortcuts, &json!({"shortcuts": []})).is_err());
    }

    #[test]
    fn kind_specific_checks() {
        let ok = json!({"url": "https://api.example.com/x", "template": "{{a.b}}"});
        assert!(validate_settings(WidgetKind::Web, &ok).is_ok());
        let http = json!({"url": "http://192.168.1.2/status", "template": "{{a}}"});
        let e = validate_settings(WidgetKind::Web, &http).unwrap_err();
        assert_eq!(e[0].field, "url");
        assert!(validate_settings(WidgetKind::Web, &json!({"url": "http://192.168.1.2/status", "template": "{{a}}", "allowHttp": true})).is_ok());
        let bad = json!({"url": "https://x.y", "template": "{{a", "valuePath": "a[", "headers": ["X-Key: 1", "nocolon"]});
        let fields: Vec<String> = validate_settings(WidgetKind::Web, &bad).unwrap_err().into_iter().map(|e| e.field).collect();
        assert_eq!(fields, vec!["headers[1]", "template", "valuePath"]);
        let clocks = json!({"clocks": [{"zone": "Europe/Paris"}, {"zone": "Mars/Olympus"}], "style": "analog"});
        let e = validate_settings(WidgetKind::WorldClock, &clocks).unwrap_err();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].field, "clocks[1].zone");
        let w: WorldClockSettings = typed(WidgetKind::WorldClock, &clocks);
        assert_eq!(w.style, ClockStyle::Analog);
        assert_eq!(w.clocks[0].label, "");
        assert_eq!(w.hours, HourFormat::Auto);
        let e = validate_settings(WidgetKind::Script, &json!({"command": "x", "interpreter": "custom", "click": "run"})).unwrap_err();
        assert_eq!(e.iter().map(|e| e.field.as_str()).collect::<Vec<_>>(), vec!["interpreterPath", "clickTarget"]);
        let ws: WeatherSettings = typed(WidgetKind::Weather, &json!({"location": "Oslo", "units": "celsius"}));
        assert_eq!(ws.units, TempUnit::Celsius);
    }

    #[test]
    fn secrets_are_stripped() {
        let mut v = json!({"url": "https://x.y", "token": "abc"});
        strip_secrets(WidgetKind::Web, &mut v);
        assert_eq!(v["token"], "");
        let mut s = json!({"command": "x"});
        strip_secrets(WidgetKind::Script, &mut s);
        assert_eq!(s, json!({"command": "x"}));
    }
}
