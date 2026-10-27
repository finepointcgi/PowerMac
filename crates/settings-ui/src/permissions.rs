//! The macOS privacy permissions PowerMac's modules need: which ones the
//! enabled modules use, their live state, the prompts that exist, and the
//! exact System Settings pane for each.
//!
//! Shared by the settings window (setup + Settings → General), the tray
//! app (startup check, tray warning) and `powermac permissions --json`.
//!
//! Live state: macOS caches some answers per process (Screen Recording,
//! Input Monitoring) until the process restarts, so the settings window asks
//! a fresh `powermac permissions --json` process every second (see
//! [`crate::set_cli_exe`]) and compares it with what the running app
//! reports in `app-status.json`: granted here but not in the app means
//! "restart PowerMac".
//!
//! The pane URLs (`x-apple.systempreferences:com.apple.preference.security?…`)
//! were checked against macOS 27.2: System Settings' Privacy & Security
//! extension (`com.apple.settings.PrivacySecurity.extension`) declares
//! `legacyBundleIdentifier = com.apple.preference.security` with
//! `allowsXAppleSystemPreferencesURLScheme`, and lists every anchor used
//! here (`Privacy_Accessibility`, `Privacy_ListenEvent`,
//! `Privacy_ScreenCapture`, `Privacy_AudioCapture`, `Privacy_Calendars`,
//! `Privacy_Reminders`, `Privacy_Camera`, `Privacy_Automation`).

use serde::{Deserialize, Serialize};

use pm_config::AppConfig;

/// A privacy permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Perm {
    Accessibility,
    InputMonitoring,
    ScreenRecording,
    /// "System Audio Recording Only" (per-app volume taps).
    AudioCapture,
    Calendars,
    Reminders,
    Camera,
    Automation,
}

impl Perm {
    pub const ALL: [Perm; 8] = [
        Perm::Accessibility,
        Perm::InputMonitoring,
        Perm::ScreenRecording,
        Perm::AudioCapture,
        Perm::Calendars,
        Perm::Reminders,
        Perm::Camera,
        Perm::Automation,
    ];

    /// Accessibility, Input Monitoring and Screen Recording are always
    /// listed; the others only when an enabled feature uses them.
    pub fn is_core(self) -> bool {
        matches!(self, Perm::Accessibility | Perm::InputMonitoring | Perm::ScreenRecording)
    }

    pub fn title(self) -> &'static str {
        match self {
            Perm::Accessibility => "Accessibility",
            Perm::InputMonitoring => "Input Monitoring",
            Perm::ScreenRecording => "Screen & System Audio Recording",
            Perm::AudioCapture => "System Audio Recording",
            Perm::Calendars => "Calendars",
            Perm::Reminders => "Reminders",
            Perm::Camera => "Camera",
            Perm::Automation => "Automation",
        }
    }

    /// Why PowerMac asks for it.
    pub fn why(self) -> &'static str {
        match self {
            Perm::Accessibility => "Move and resize other apps' windows, react to shortcuts and type for you.",
            Perm::InputMonitoring => "See the keyboard and mouse to share them with another computer and for hotkeys.",
            Perm::ScreenRecording => "Read text on screen and show live window thumbnails. Nothing is recorded or sent anywhere.",
            Perm::AudioCapture => "Set a separate volume for each app. Nothing is recorded.",
            Perm::Calendars => "Show your next events in the notch.",
            Perm::Reminders => "Show and tick off reminders in the notch.",
            Perm::Camera => "Show the camera when you click the notch mirror.",
            Perm::Automation => "Ask apps for details (browser tabs, the current track) when you turn that on.",
        }
    }

    /// The anchor of the Privacy & Security pane.
    pub fn pane(self) -> &'static str {
        match self {
            Perm::Accessibility => "Privacy_Accessibility",
            Perm::InputMonitoring => "Privacy_ListenEvent",
            Perm::ScreenRecording => "Privacy_ScreenCapture",
            Perm::AudioCapture => "Privacy_AudioCapture",
            Perm::Calendars => "Privacy_Calendars",
            Perm::Reminders => "Privacy_Reminders",
            Perm::Camera => "Privacy_Camera",
            Perm::Automation => "Privacy_Automation",
        }
    }

    /// The URL that opens exactly this pane of System Settings.
    pub fn settings_url(self) -> String {
        format!("x-apple.systempreferences:com.apple.preference.security?{}", self.pane())
    }

    /// Whether a system prompt can be shown for it from here.
    pub fn can_request(self) -> bool {
        matches!(self, Perm::Accessibility | Perm::InputMonitoring | Perm::ScreenRecording | Perm::AudioCapture)
    }

    /// The process must restart before a new grant takes effect.
    pub fn needs_restart(self) -> bool {
        matches!(self, Perm::InputMonitoring | Perm::ScreenRecording | Perm::AudioCapture)
    }

    pub fn icon(self) -> &'static str {
        use crate::theme::icon;
        match self {
            Perm::Accessibility => icon::CURSOR_CLICK,
            Perm::InputMonitoring => icon::KEYBOARD,
            Perm::ScreenRecording => icon::MONITOR,
            Perm::AudioCapture => icon::SPEAKER_HIGH,
            Perm::Calendars => icon::CLOCK,
            Perm::Reminders => icon::CHECK_CIRCLE,
            Perm::Camera => icon::EYE,
            Perm::Automation => icon::MAGIC_WAND,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Perm::Accessibility => "accessibility",
            Perm::InputMonitoring => "input-monitoring",
            Perm::ScreenRecording => "screen-recording",
            Perm::AudioCapture => "audio-capture",
            Perm::Calendars => "calendars",
            Perm::Reminders => "reminders",
            Perm::Camera => "camera",
            Perm::Automation => "automation",
        }
    }
}

/// The state of one permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Grant {
    Granted,
    Denied,
    /// Never asked.
    NotDetermined,
    /// Can't be read from here (the app reports it while it runs, or it's
    /// asked the first time a feature uses it).
    #[default]
    Unknown,
    /// Not a thing on this platform.
    NotApplicable,
}

impl Grant {
    pub fn granted(self) -> bool {
        matches!(self, Grant::Granted | Grant::NotApplicable)
    }

    pub fn from_bool(b: bool) -> Grant {
        if b {
            Grant::Granted
        } else {
            Grant::Denied
        }
    }

    /// The Notch Shelf's status strings ("granted", "denied", "notDetermined", …).
    pub fn from_notch(s: &str) -> Grant {
        match s {
            "granted" => Grant::Granted,
            "denied" | "restricted" => Grant::Denied,
            "notDetermined" => Grant::NotDetermined,
            _ => Grant::Unknown,
        }
    }
}

/// What a process can see of its own permissions (no prompts).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Probe {
    pub accessibility: Grant,
    pub input_monitoring: Grant,
    pub screen_recording: Grant,
    pub audio_capture: Grant,
}

impl Probe {
    pub fn get(&self, p: Perm) -> Grant {
        match p {
            Perm::Accessibility => self.accessibility,
            Perm::InputMonitoring => self.input_monitoring,
            Perm::ScreenRecording => self.screen_recording,
            Perm::AudioCapture => self.audio_capture,
            _ => Grant::Unknown,
        }
    }

    /// Everything granted (tests, screenshots).
    pub fn all_granted() -> Probe {
        Probe { accessibility: Grant::Granted, input_monitoring: Grant::Granted, screen_recording: Grant::Granted, audio_capture: Grant::Granted }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn AXIsProcessTrusted() -> bool;
        pub fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGPreflightListenEventAccess() -> bool;
        pub fn CGRequestListenEventAccess() -> bool;
        pub fn CGPreflightScreenCaptureAccess() -> bool;
        pub fn CGRequestScreenCaptureAccess() -> bool;
    }
}

/// This process' permissions, right now (no prompt).
pub fn probe_in_process() -> Probe {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: argument-less C functions.
        let (ax, listen, screen) =
            unsafe { (mac::AXIsProcessTrusted(), mac::CGPreflightListenEventAccess(), mac::CGPreflightScreenCaptureAccess()) };
        let audio = match audio_control::capture_permission() {
            audio_control::CapturePermission::Granted => Grant::Granted,
            audio_control::CapturePermission::Denied => Grant::Denied,
            audio_control::CapturePermission::NotDetermined => Grant::NotDetermined,
            audio_control::CapturePermission::NotApplicable => Grant::Unknown,
        };
        Probe {
            accessibility: Grant::from_bool(ax),
            input_monitoring: Grant::from_bool(listen),
            screen_recording: Grant::from_bool(screen),
            audio_capture: audio,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Probe {
            accessibility: Grant::NotApplicable,
            input_monitoring: Grant::NotApplicable,
            screen_recording: Grant::NotApplicable,
            audio_capture: Grant::NotApplicable,
        }
    }
}

/// Show the system prompt for `p` where one exists (returns whether it is
/// granted now; most prompts return before the user answers). Audio
/// capture blocks until answered: call it off the UI thread.
pub fn request(p: Perm) -> bool {
    #[cfg(target_os = "macos")]
    {
        match p {
            Perm::Accessibility => {
                use objc2_foundation::{NSDictionary, NSNumber, NSString};
                let key = NSString::from_str("AXTrustedCheckOptionPrompt");
                let yes = NSNumber::new_bool(true);
                let dict = NSDictionary::from_slices(&[&*key], &[&*yes]);
                let ptr = objc2::rc::Retained::as_ptr(&dict) as *const std::ffi::c_void;
                // SAFETY: a CFDictionary (toll-free bridged NSDictionary) alive for the call.
                unsafe { mac::AXIsProcessTrustedWithOptions(ptr) }
            }
            // SAFETY: argument-less C functions.
            Perm::InputMonitoring => unsafe { mac::CGRequestListenEventAccess() },
            Perm::ScreenRecording => unsafe { mac::CGRequestScreenCaptureAccess() },
            Perm::AudioCapture => audio_control::request_capture_permission().unwrap_or(false),
            _ => false,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = p;
        true
    }
}

/// Open System Settings on `p`'s pane.
pub fn open_pane(p: Perm) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(p.settings_url()).spawn();
    #[cfg(not(target_os = "macos"))]
    let _ = p;
}

/// One permission and the enabled features that use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub perm: Perm,
    /// Feature names; empty for a core permission no enabled feature uses.
    pub features: Vec<&'static str>,
}

/// Which permissions the enabled modules of `c` need. The core ones are
/// always listed (possibly without features); the optional ones only when
/// something uses them.
pub fn needs(c: &AppConfig) -> Vec<Need> {
    let mut ax: Vec<&'static str> = Vec::new();
    let mut input = Vec::new();
    let mut screen = Vec::new();
    let mut other: Vec<Need> = Vec::new();
    let km = &c.keyboard_manager;
    let mu = &c.mouse_utils;
    let mouse_utils_on = mu.find_my_mouse.enabled || mu.highlighter.enabled || mu.crosshairs.enabled;
    if c.mwb.enabled {
        ax.push("Mouse Without Borders");
        input.push("Mouse Without Borders");
    }
    if c.fancyzones.enabled {
        ax.push("FancyZones");
        input.push("FancyZones");
    }
    if c.window_switcher.enabled {
        ax.push("Window Switcher");
        if c.window_switcher.thumbnails {
            screen.push("Window Switcher thumbnails");
        }
    }
    if c.dock_previews.enabled {
        ax.push("Dock Previews");
        screen.push("Dock Previews thumbnails");
    }
    if c.menu_bar.enabled {
        ax.push("Menu Bar");
    }
    if c.workspaces.enabled {
        ax.push("Workspaces");
    }
    if km.enabled || km.paste_plain_text.enabled || km.hyper_key.is_on() {
        ax.push("Keyboard Manager");
        input.push("Keyboard Manager");
    }
    if c.text_expander.enabled {
        ax.push("Text Expansion");
    }
    if mouse_utils_on {
        ax.push("Mouse Utilities");
        input.push("Mouse Utilities");
    }
    if c.scroll_reverser.enabled {
        ax.push("Scrolling");
    }
    if c.mouse_tuning.enabled && c.mouse_tuning.needs_button_hook() {
        ax.push("Mouse buttons");
    }
    if c.display_control.enabled && (c.display_control.take_over_brightness_keys || c.display_control.take_over_volume_keys) {
        ax.push("Brightness and volume keys");
    }
    if c.clipboard_history.enabled && c.clipboard_history.hotkey().is_some() {
        ax.push("Clipboard History shortcut");
    }
    if c.text_extractor.enabled {
        screen.push("Text Extractor");
    }
    if c.audio.enabled && c.audio.per_app_volume {
        other.push(Need { perm: Perm::AudioCapture, features: vec!["Per-app volume"] });
    }
    let n = &c.notch_shelf;
    if n.enabled {
        use notch_shelf::WidgetKind as K;
        for (perm, kind, name) in [
            (Perm::Calendars, K::Calendar, "Notch Calendar widget"),
            (Perm::Reminders, K::Reminders, "Notch Reminders widget"),
            (Perm::Camera, K::Mirror, "Notch Mirror widget"),
        ] {
            if n.instances_of(kind).next().is_some() {
                other.push(Need { perm, features: vec![name] });
            }
        }
    }
    let mut auto = Vec::new();
    if c.workspaces.enabled && c.workspaces.capture.browser_urls {
        auto.push("Workspaces browser tabs");
    }
    if n.enabled && n.instances_of(notch_shelf::WidgetKind::NowPlaying).any(|w| w.settings["useApplescript"] == true) {
        auto.push("Now Playing from Music and Spotify");
    }
    if !auto.is_empty() {
        other.push(Need { perm: Perm::Automation, features: auto });
    }
    let mut v = vec![
        Need { perm: Perm::Accessibility, features: ax },
        Need { perm: Perm::InputMonitoring, features: input },
        Need { perm: Perm::ScreenRecording, features: screen },
    ];
    v.extend(other);
    v
}

/// Core permissions that an enabled feature needs and `probe` says are
/// missing (what makes the tray app open the setup at startup).
pub fn missing_needed(c: &AppConfig, probe: &Probe) -> Vec<Perm> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    needs(c)
        .into_iter()
        .filter(|n| n.perm.is_core() && !n.features.is_empty() && !probe.get(n.perm).granted())
        .map(|n| n.perm)
        .collect()
}

/// What a permission row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Granted,
    /// Granted, but the running app still has the old answer: restart it.
    NeedsRestart,
    Missing,
    /// Still not granted after the user turned it on: an outdated entry
    /// (an older build's signature) is in the list.
    Stale,
    /// Not readable from here (asked when the feature first runs).
    Unknown,
}

/// Combine the live state, what the running app sees (`None`: not
/// running / not reported) and whether the user said it's already on.
pub fn row_state(perm: Perm, live: Grant, app: Option<bool>, user_says_on: bool) -> RowState {
    match live {
        Grant::Granted | Grant::NotApplicable => {
            if app == Some(false) && perm.is_core() {
                RowState::NeedsRestart
            } else {
                RowState::Granted
            }
        }
        Grant::Unknown => RowState::Unknown,
        Grant::Denied | Grant::NotDetermined => {
            if user_says_on {
                RowState::Stale
            } else {
                RowState::Missing
            }
        }
    }
}

/// The guidance for an outdated entry, as numbered steps.
pub fn stale_steps(p: Perm) -> [String; 4] {
    [
        format!("Open System Settings → Privacy & Security → {}.", p.title()),
        "Select every PowerMac entry and remove it with the − button (an older build left it behind; macOS ties the switch to that build's signature).".into(),
        "Click +, choose PowerMac in Applications, and turn it on.".into(),
        "Click Restart PowerMac below so the app picks up the new permission.".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_metadata() {
        assert_eq!(Perm::Accessibility.settings_url(), "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
        assert_eq!(Perm::InputMonitoring.pane(), "Privacy_ListenEvent");
        assert_eq!(Perm::ScreenRecording.pane(), "Privacy_ScreenCapture");
        assert_eq!(Perm::AudioCapture.pane(), "Privacy_AudioCapture");
        assert_eq!(Perm::Automation.pane(), "Privacy_Automation");
        for p in Perm::ALL {
            assert!(!p.why().is_empty() && !p.title().is_empty() && !p.slug().is_empty());
        }
        let j = serde_json::to_string(&Probe { accessibility: Grant::Granted, ..Default::default() }).unwrap();
        assert!(j.contains("\"accessibility\":\"granted\"") && j.contains("\"inputMonitoring\":\"unknown\""), "{j}");
    }

    #[test]
    fn needs_follow_the_config() {
        let mut c = AppConfig::default();
        c.notch_shelf.enabled = false;
        c.audio.enabled = false;
        let n = needs(&c);
        assert_eq!(n.iter().map(|n| n.perm).collect::<Vec<_>>()[..3], [Perm::Accessibility, Perm::InputMonitoring, Perm::ScreenRecording]);
        assert!(n[0].features.contains(&"FancyZones") && n[0].features.contains(&"Mouse Without Borders"));
        assert!(!n.iter().any(|n| n.perm == Perm::AudioCapture));
        c.audio.enabled = true;
        c.audio.per_app_volume = true;
        c.workspaces.capture.browser_urls = true;
        let n = needs(&c);
        assert!(n.iter().any(|n| n.perm == Perm::AudioCapture));
        assert!(n.iter().any(|n| n.perm == Perm::Automation && n.features == ["Workspaces browser tabs"]));
        // Nothing on: core permissions listed without features, none missing.
        let mut off = AppConfig::default();
        off.mwb.enabled = false;
        off.fancyzones.enabled = false;
        off.scroll_reverser.enabled = false;
        off.workspaces.enabled = false;
        off.window_switcher.enabled = false;
        off.dock_previews.enabled = false;
        off.text_extractor.enabled = false;
        off.keyboard_manager = keyboard_manager::Config::default();
        off.keyboard_manager.enabled = false;
        off.keyboard_manager.paste_plain_text.enabled = false;
        off.keyboard_manager.hyper_key.mode = keyboard_manager::HyperMode::Off;
        off.clipboard_history.enabled = false;
        off.mouse_utils = Default::default();
        off.mouse_utils.find_my_mouse.enabled = false;
        off.mouse_utils.highlighter.enabled = false;
        off.mouse_utils.crosshairs.enabled = false;
        off.display_control.enabled = false;
        off.mouse_tuning.enabled = false;
        assert!(missing_needed(&off, &Probe::default()).is_empty(), "{:?}", needs(&off));
        if cfg!(target_os = "macos") {
            assert_eq!(missing_needed(&c, &Probe::default()), [Perm::Accessibility, Perm::InputMonitoring, Perm::ScreenRecording]);
            assert!(missing_needed(&c, &Probe::all_granted()).is_empty());
        }
    }

    #[test]
    fn row_states() {
        use RowState::*;
        assert_eq!(row_state(Perm::ScreenRecording, Grant::Granted, Some(true), false), Granted);
        assert_eq!(row_state(Perm::ScreenRecording, Grant::Granted, None, false), Granted);
        assert_eq!(row_state(Perm::ScreenRecording, Grant::Granted, Some(false), false), NeedsRestart);
        assert_eq!(row_state(Perm::ScreenRecording, Grant::Denied, Some(false), false), Missing);
        assert_eq!(row_state(Perm::ScreenRecording, Grant::Denied, None, true), Stale);
        assert_eq!(row_state(Perm::Calendars, Grant::Unknown, None, false), Unknown);
        assert!(stale_steps(Perm::ScreenRecording)[0].contains("Screen & System Audio Recording"));
    }
}
