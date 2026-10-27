//! Multi-finger trackpad gestures that run automation actions
//! (BetterTouchTool-style): 3/4/5-finger swipes, 3/4/5-finger taps and
//! clicks, and pinches, each bound to a shared
//! [`keyboard_manager::automation::Action`] (shell command, shortcut, text,
//! program, URL, ...), optionally only in one app.
//!
//! ```no_run
//! use gestures::{Gestures, GesturesConfig};
//! let cfg: GesturesConfig = serde_json::from_str(r#"{
//!   "bindings": [
//!     {"gesture": "3-finger-tap", "action": {"type": "shortcut", "keys": "Cmd+W"}},
//!     {"gesture": "4-finger-swipe-down", "action": {"type": "shell", "command": "pmset displaysleepnow"}},
//!     {"gesture": "3-finger-click", "app": "com.google.Chrome",
//!      "action": {"type": "shortcut", "keys": "Cmd+Shift+T"}}
//!   ]
//! }"#)?;
//! let handle = Gestures::start(cfg);
//! println!("{:?}", handle.status());   // Running / Unavailable("…") / Off
//! # anyhow::Ok(())
//! ```
//!
//! # Config
//!
//! JSON camelCase, every field optional; nothing is bound by default (and
//! with no bindings the trackpad isn't read at all).
//!
//! | field | default | |
//! |---|---|---|
//! | `enabled` | `true` | master switch |
//! | `bindings` | `[]` | `{gesture, action, app?, enabled?}` |
//! | `swallowClicks` | `true` | a physical click that triggers a bound N-finger click doesn't also click in the app |
//! | `thresholds` | see [`Thresholds`] | recognizer tuning |
//!
//! Gestures are written `"3-finger-swipe-left"`, `"4-finger-swipe-up"`,
//! `"3-finger-tap"`, `"5-finger-click"`, `"4-finger-pinch-in"`,
//! `"2-finger-pinch-out"` (case and separators are flexible: `"swipe 3
//! left"`, `"three_finger_tap"`). Swipes take 3–5 fingers, taps and clicks
//! 3–5, pinches 2–5.
//!
//! App scoping works like Keyboard Manager's: `"app"` is a bundle id or app
//! name (macOS) / exe name (Windows), case-insensitive. A binding for the
//! frontmost app wins over a global one for the same gesture; bind
//! `{"type": "disabled"}` for an app to turn a global binding off there.
//!
//! # macOS's own gestures
//!
//! Touches are read (never intercepted) from the private
//! MultitouchSupport framework, so macOS's own gestures still happen: the
//! 3/4-finger swipes between spaces / Mission Control / App Exposé, the
//! 4/5-finger pinches for Launchpad / Show Desktop, three-finger drag and
//! "Look up" (3-finger tap) in System Settings → Trackpad (and
//! Accessibility → Pointer Control → Trackpad Options for three-finger
//! drag) **also fire** when you bind the same gesture here. Turn the macOS
//! one off there if you want only yours; this crate never changes those
//! settings. A gesture you don't bind does nothing here.
//!
//! Only N-finger *clicks* can be kept from the app (`swallowClicks`, via the
//! shared input hook at [`PRIORITY`]; needs Accessibility). If the
//! framework can't be loaded (a future macOS, Windows),
//! [`Handle::status`] reports [`Status::Unavailable`] and nothing else is
//! affected.
//!
//! # Windows
//!
//! Not implemented (precision-touchpad gestures would need Raw Input HID
//! parsing): [`availability`] is `Unavailable` and the handle is inert.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use keyboard_manager::automation::{Action, Outcome};

pub mod recognizer;
pub use recognizer::{Contact, Frame, Recognizer, Thresholds};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::reader;

mod engine;
pub use engine::{GestureEvent, Gestures, Handle, Listener};

/// Where the click subscriber sits in the shared input hook: after mouse
/// tuning (700) and before the text expander (600); see
/// docs/architecture/input-hook.md.
pub const PRIORITY: i32 = pm_hook::priority::GESTURES;

// ---------------------------------------------------------------------------
// Gestures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwipeDir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PinchDir {
    In,
    Out,
}

/// A recognized gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gesture {
    Swipe { fingers: u8, dir: SwipeDir },
    Tap { fingers: u8 },
    Click { fingers: u8 },
    Pinch { fingers: u8, dir: PinchDir },
}

impl Gesture {
    /// Whether this finger count is supported for the kind.
    pub fn is_valid(&self) -> bool {
        match *self {
            Gesture::Swipe { fingers, .. }
            | Gesture::Tap { fingers }
            | Gesture::Click { fingers } => (3..=5).contains(&fingers),
            Gesture::Pinch { fingers, .. } => (2..=5).contains(&fingers),
        }
    }

    pub fn fingers(&self) -> u8 {
        match *self {
            Gesture::Swipe { fingers, .. }
            | Gesture::Tap { fingers }
            | Gesture::Click { fingers }
            | Gesture::Pinch { fingers, .. } => fingers,
        }
    }

    /// Every bindable gesture, for a settings drop-down.
    pub fn all() -> Vec<Gesture> {
        let mut v = Vec::new();
        for n in 3..=5 {
            for dir in [
                SwipeDir::Left,
                SwipeDir::Right,
                SwipeDir::Up,
                SwipeDir::Down,
            ] {
                v.push(Gesture::Swipe { fingers: n, dir });
            }
        }
        for n in 3..=5 {
            v.push(Gesture::Tap { fingers: n });
        }
        for n in 3..=5 {
            v.push(Gesture::Click { fingers: n });
        }
        for n in 2..=5 {
            v.push(Gesture::Pinch {
                fingers: n,
                dir: PinchDir::In,
            });
            v.push(Gesture::Pinch {
                fingers: n,
                dir: PinchDir::Out,
            });
        }
        v
    }

    /// "Three-finger swipe left".
    pub fn label(&self) -> String {
        let n = match self.fingers() {
            2 => "Two",
            3 => "Three",
            4 => "Four",
            5 => "Five",
            _ => "Multi",
        };
        match self {
            Gesture::Swipe { dir, .. } => format!("{n}-finger swipe {}", dir_word(*dir)),
            Gesture::Tap { .. } => format!("{n}-finger tap"),
            Gesture::Click { .. } => format!("{n}-finger click"),
            Gesture::Pinch { dir, .. } => {
                format!(
                    "{n}-finger pinch {}",
                    if *dir == PinchDir::In { "in" } else { "out" }
                )
            }
        }
    }

    /// True for gestures macOS itself may use (System Settings → Trackpad),
    /// so a settings page can warn that both will happen.
    pub fn may_conflict_with_macos(&self) -> bool {
        match *self {
            Gesture::Swipe { fingers, .. } => fingers == 3 || fingers == 4,
            Gesture::Tap { fingers } => fingers == 3,
            Gesture::Pinch { fingers, .. } => fingers == 2 || fingers >= 4,
            Gesture::Click { .. } => false,
        }
    }
}

fn dir_word(d: SwipeDir) -> &'static str {
    match d {
        SwipeDir::Left => "left",
        SwipeDir::Right => "right",
        SwipeDir::Up => "up",
        SwipeDir::Down => "down",
    }
}

impl fmt::Display for Gesture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.fingers();
        match self {
            Gesture::Swipe { dir, .. } => write!(f, "{n}-finger-swipe-{}", dir_word(*dir)),
            Gesture::Tap { .. } => write!(f, "{n}-finger-tap"),
            Gesture::Click { .. } => write!(f, "{n}-finger-click"),
            Gesture::Pinch { dir, .. } => {
                write!(
                    f,
                    "{n}-finger-pinch-{}",
                    if *dir == PinchDir::In { "in" } else { "out" }
                )
            }
        }
    }
}

impl FromStr for Gesture {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let lower = s.trim().to_ascii_lowercase();
        let mut fingers = None;
        let mut kind = None;
        let mut dir: Option<String> = None;
        for tok in lower
            .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
            .filter(|t| !t.is_empty())
        {
            // "3finger", "swipe3"
            let digits: String = tok.chars().filter(|c| c.is_ascii_digit()).collect();
            let word: String = tok.chars().filter(|c| !c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                fingers = Some(
                    digits
                        .parse::<u8>()
                        .map_err(|_| format!("bad finger count in '{s}'"))?,
                );
            }
            match word.as_str() {
                "" | "finger" | "fingers" => {}
                "two" => fingers = Some(2),
                "three" => fingers = Some(3),
                "four" => fingers = Some(4),
                "five" => fingers = Some(5),
                "swipe" | "tap" | "click" | "pinch" => kind = Some(word.clone()),
                "left" | "right" | "up" | "down" | "in" | "out" => dir = Some(word.clone()),
                other => return Err(format!("unknown word '{other}' in gesture '{s}'")),
            }
        }
        let fingers = fingers.ok_or_else(|| format!("gesture '{s}' needs a finger count"))?;
        let g = match (kind.as_deref(), dir.as_deref()) {
            (Some("swipe"), Some(d @ ("left" | "right" | "up" | "down"))) => Gesture::Swipe {
                fingers,
                dir: match d {
                    "left" => SwipeDir::Left,
                    "right" => SwipeDir::Right,
                    "up" => SwipeDir::Up,
                    _ => SwipeDir::Down,
                },
            },
            (Some("pinch"), Some(d @ ("in" | "out"))) => Gesture::Pinch {
                fingers,
                dir: if d == "in" {
                    PinchDir::In
                } else {
                    PinchDir::Out
                },
            },
            (Some("tap"), None) => Gesture::Tap { fingers },
            (Some("click"), None) => Gesture::Click { fingers },
            _ => {
                return Err(format!(
                    "can't read gesture '{s}' (e.g. \"3-finger-swipe-left\")"
                ))
            }
        };
        if !g.is_valid() {
            return Err(format!(
                "{} is not supported (swipes, taps and clicks: 3-5 fingers; pinches: 2-5)",
                g.label()
            ));
        }
        Ok(g)
    }
}

impl Serialize for Gesture {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Gesture {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Gesture settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GesturesConfig {
    pub enabled: bool,
    pub bindings: Vec<GestureBinding>,
    pub swallow_clicks: bool,
    pub thresholds: Thresholds,
}

impl Default for GesturesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bindings: Vec::new(),
            swallow_clicks: true,
            thresholds: Thresholds::default(),
        }
    }
}

fn yes() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}

/// One gesture → action binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GestureBinding {
    pub gesture: Gesture,
    pub action: Action,
    /// Only while this app is frontmost (bundle id / name / exe name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

impl GestureBinding {
    pub fn new(gesture: Gesture, action: Action) -> Self {
        Self {
            gesture,
            action,
            app: None,
            enabled: true,
        }
    }
}

impl GesturesConfig {
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("config serializes")
    }

    fn active(&self) -> impl Iterator<Item = &GestureBinding> {
        self.bindings
            .iter()
            .filter(move |b| self.enabled && b.enabled)
    }

    /// The action for `gesture` with this frontmost app (see
    /// [`keyboard_manager::foreground_app`]): an app-scoped binding first,
    /// else a global one. `Some(Action::Disabled)` means "explicitly
    /// nothing here".
    pub fn resolve(&self, gesture: Gesture, front_app: &[String]) -> Option<&Action> {
        keyboard_manager::automation::resolve_scoped(
            self.active().filter(|b| b.gesture == gesture),
            |b| b.app.as_deref(),
            front_app,
        )
        .map(|b| &b.action)
    }

    /// Something is bound (the trackpad needs reading).
    pub fn needs_reader(&self) -> bool {
        self.active().any(|b| b.action != Action::Disabled)
    }

    /// Taps or clicks are bound: physical clicks must be watched (to tell
    /// a tap from a click, and to fire / swallow N-finger clicks).
    pub fn needs_click_hook(&self) -> bool {
        self.active()
            .any(|b| matches!(b.gesture, Gesture::Tap { .. } | Gesture::Click { .. }))
    }

    /// Finger counts with a click binding, and whether any is app-scoped.
    pub(crate) fn click_bindings(&self) -> (u32, bool) {
        let mut mask = 0u32;
        let mut scoped = false;
        for b in self.active() {
            if let Gesture::Click { fingers } = b.gesture {
                mask |= 1 << fingers;
                scoped |= b.app.as_deref().is_some_and(|a| !a.trim().is_empty());
            }
        }
        (mask, scoped)
    }

    /// Problems that make a binding not work.
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, b) in self.bindings.iter().enumerate() {
            for e in b.action.validate() {
                out.push(format!("bindings[{i}] ({}): {e}", b.gesture));
            }
            if b.app.as_deref().is_some_and(|a| a.trim().is_empty()) {
                out.push(format!("bindings[{i}]: empty app name"));
            }
            for c in &self.bindings[..i] {
                if c.gesture == b.gesture && c.app == b.app && c.enabled && b.enabled {
                    out.push(format!(
                        "bindings[{i}]: {} is bound twice (the first one wins)",
                        b.gesture
                    ));
                    break;
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// A multitouch device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// `MTDeviceGetDeviceID` (0 when unknown).
    pub id: u64,
    pub built_in: bool,
    /// `MTDeviceGetFamilyID` (-1 when unknown).
    pub family_id: i32,
}

/// What the gesture engine is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Disabled, or nothing bound.
    Off,
    /// Reading `devices` trackpads. `clicks`: the click watcher's state
    /// (`None` when no tap/click is bound).
    Running {
        devices: Vec<DeviceInfo>,
        clicks: Option<Result<(), String>>,
    },
    /// "gestures unavailable": the framework couldn't be loaded / started.
    Unavailable(String),
}

/// Whether trackpad gestures can work here (loads the framework; cheap).
pub fn availability() -> Result<Vec<DeviceInfo>, String> {
    #[cfg(target_os = "macos")]
    return macos::availability();
    #[cfg(not(target_os = "macos"))]
    return Err(UNSUPPORTED.into());
}

#[cfg(not(target_os = "macos"))]
pub(crate) const UNSUPPORTED: &str =
    "gestures unavailable: trackpad gestures are only supported on macOS";

#[cfg(test)]
mod tests;
