//! The editable state behind the settings window, and everything about it
//! that isn't drawing: text buffers for values that need parsing, validation,
//! the MWB matrix operations, and turning the draft back into config files.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Local};
use serde::Serialize;

use keyboard_manager::config::{KeyRemap, RunProgram, ShortcutRemap, Target};
use keyboard_manager::keys::{key_name, mod_family, parse_combo, parse_key, Key, ModFamily, Shortcut};
use pm_config::AppConfig;

pub use mwb::matrix::MAX_MACHINE;

/// The sidebar pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Page {
    General,
    Mwb,
    FancyZones,
    Awake,
    /// "Scrolling": reversal and smooth scrolling.
    ScrollReverser,
    MouseUtils,
    KeyboardManager,
    /// "Mouse": pointer acceleration / speed and button mappings.
    MouseTuning,
    Displays,
    ClipboardHistory,
    NotchShelf,
    /// The Notch Shelf's Home view: pages, widget instances and their
    /// settings, the widget gallery, layout import / export.
    NotchWidgets,
    /// Floating shelves opened by shaking the pointer while dragging.
    DragShelf,
    ColorPicker,
    TextExtractor,
    FileTools,
    WindowSwitcher,
    /// Default-device rules and per-app volume.
    Sound,
    DockPreviews,
    TextExpander,
    MenuBar,
    /// Saved desktops of apps and windows.
    Workspaces,
    /// Sound's mixer matrix: sources → outputs, EQ, limiter, meters.
    Mixer,
    /// Battery health and the charge limit.
    Battery,
    /// Hotkeys, mouse buttons and trackpad gestures that run actions.
    Automation,
    /// Live per-app connections and the firewall rules model.
    Network,
    /// The first-run setup (welcome, features, permissions, tips). Not in
    /// the sidebar: it takes the whole window.
    Welcome,
}

impl Page {
    /// Sidebar order (grouped by [`Page::section`]).
    pub const ALL: [Page; 26] = [
        Page::General,
        Page::FancyZones,
        Page::WindowSwitcher,
        Page::Workspaces,
        Page::NotchShelf,
        Page::NotchWidgets,
        Page::DragShelf,
        Page::DockPreviews,
        Page::MenuBar,
        Page::MouseTuning,
        Page::ScrollReverser,
        Page::MouseUtils,
        Page::KeyboardManager,
        Page::Automation,
        Page::TextExpander,
        Page::Displays,
        Page::Sound,
        Page::Mixer,
        Page::Battery,
        Page::Awake,
        Page::ClipboardHistory,
        Page::ColorPicker,
        Page::TextExtractor,
        Page::FileTools,
        Page::Network,
        Page::Mwb,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Mwb => "Mouse Without Borders",
            Page::FancyZones => "FancyZones",
            Page::Awake => "Awake",
            Page::ScrollReverser => "Scrolling",
            Page::MouseUtils => "Mouse Utilities",
            Page::KeyboardManager => "Keyboard Manager",
            Page::MouseTuning => "Mouse",
            Page::Displays => "Displays",
            Page::ClipboardHistory => "Clipboard History",
            Page::NotchShelf => "Notch Shelf",
            Page::NotchWidgets => "Notch Widgets",
            Page::DragShelf => "Drag Shelf",
            Page::ColorPicker => "Color Picker",
            Page::TextExtractor => "Text Extractor",
            Page::FileTools => "File Tools",
            Page::WindowSwitcher => "Window Switcher",
            Page::Sound => "Sound",
            Page::DockPreviews => "Dock Previews",
            Page::TextExpander => "Text Expansion",
            Page::MenuBar => "Menu Bar",
            Page::Workspaces => "Workspaces",
            Page::Mixer => "Mixer",
            Page::Battery => "Battery",
            Page::Automation => "Automation",
            Page::Network => "Network",
            Page::Welcome => "Welcome",
        }
    }

    /// The page's name on the command line (`powermac settings --page
    /// notch-widgets`): its title in lowercase with dashes.
    pub fn slug(self) -> String {
        self.title().to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-")
    }

    /// The page named `name`: its slug, its title or its internal name, in
    /// any case, with or without dashes / spaces ("notch-widgets",
    /// "Notch Widgets", "NotchWidgets", "notchwidgets").
    pub fn from_name(name: &str) -> Option<Page> {
        let squash = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        let n = squash(name);
        if n.is_empty() {
            return None;
        }
        if matches!(n.as_str(), "welcome" | "setup" | "onboarding" | "permissions" | "setuppermissions") {
            return Some(Page::Welcome);
        }
        Page::ALL.into_iter().find(|p| squash(p.title()) == n || squash(&format!("{p:?}")) == n)
    }

    /// The sidebar group the page is listed under (`None`: above the groups).
    pub fn section(self) -> Option<&'static str> {
        Some(match self {
            Page::General | Page::Welcome => return None,
            Page::FancyZones | Page::WindowSwitcher | Page::Workspaces | Page::NotchShelf | Page::NotchWidgets | Page::DragShelf | Page::DockPreviews | Page::MenuBar => {
                "Windows & Desktop"
            }
            Page::MouseTuning
            | Page::ScrollReverser
            | Page::MouseUtils
            | Page::KeyboardManager
            | Page::Automation
            | Page::TextExpander => "Mouse & Keyboard",
            Page::Displays | Page::Sound | Page::Mixer | Page::Battery | Page::Awake => "Displays & Power",
            Page::Network => "Security & Network",
            Page::ClipboardHistory | Page::ColorPicker | Page::TextExtractor | Page::FileTools => {
                "Productivity"
            }
            Page::Mwb => "Connectivity",
        })
    }
}

/// A validation problem that blocks saving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub page: Page,
    pub message: String,
}

fn problem(page: Page, message: impl Into<String>) -> Problem {
    Problem { page, message: message.into() }
}

// ---------------------------------------------------------------------------
// Field validation (also shown inline next to the fields)

pub fn validate_key(key: &str) -> Option<String> {
    let k = mwb::normalize_key(key);
    if !mwb::crypto::is_key_valid(&k) {
        return Some(format!("The security key needs at least 16 characters (has {}).", k.chars().count()));
    }
    None
}

pub fn validate_machine_name(name: &str) -> Option<String> {
    let n = name.trim();
    if n.chars().count() > 32 {
        return Some("Machine names can be at most 32 characters.".into());
    }
    if !n.is_ascii() {
        return Some("Machine names must be plain ASCII.".into());
    }
    if n.contains('.') {
        return Some("Leave out the domain (no '.').".into());
    }
    None
}

// ---------------------------------------------------------------------------
// MWB matrix

/// Swap slot `i` with the one before it.
pub fn matrix_move_up(slots: &mut [String; MAX_MACHINE], i: usize) {
    if i > 0 && i < MAX_MACHINE {
        slots.swap(i, i - 1);
    }
}

/// Swap slot `i` with the one after it.
pub fn matrix_move_down(slots: &mut [String; MAX_MACHINE], i: usize) {
    if i + 1 < MAX_MACHINE {
        slots.swap(i, i + 1);
    }
}

/// Move the machine in slot `from` to slot `to` (drag and drop); the
/// slots in between shift by one.
pub fn matrix_move(slots: &mut [String; MAX_MACHINE], from: usize, to: usize) {
    if from >= MAX_MACHINE || to >= MAX_MACHINE || from == to {
        return;
    }
    if from < to {
        slots[from..=to].rotate_left(1);
    } else {
        slots[to..=from].rotate_right(1);
    }
}

pub fn matrix_from_vec(names: &[String]) -> [String; MAX_MACHINE] {
    let mut s: [String; MAX_MACHINE] = Default::default();
    for (i, n) in names.iter().take(MAX_MACHINE).enumerate() {
        s[i] = n.trim().to_string();
    }
    s
}

/// Slots as stored in `config.json`: trimmed, trailing empty slots dropped
/// (inner empty slots are kept: their position matters in the 2x2 layout).
pub fn matrix_to_vec(slots: &[String; MAX_MACHINE]) -> Vec<String> {
    let mut v: Vec<String> = slots.iter().map(|s| s.trim().to_string()).collect();
    while v.last().is_some_and(|s| s.is_empty()) {
        v.pop();
    }
    v
}

pub fn validate_matrix(slots: &[String; MAX_MACHINE]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, s) in slots.iter().enumerate() {
        if let Some(e) = validate_machine_name(s) {
            out.push(format!("Slot {}: {e}", i + 1));
        }
        let t = s.trim();
        if !t.is_empty() && slots[..i].iter().any(|o| o.trim().eq_ignore_ascii_case(t)) {
            out.push(format!("{t} is in the matrix twice."));
        }
    }
    out
}

/// Whether `slot` names this machine.
pub fn is_this_machine(slot: &str, this: &str) -> bool {
    !slot.trim().is_empty() && slot.trim().eq_ignore_ascii_case(this.trim())
}

/// Put `name` in the first empty slot (if it isn't in the matrix already).
pub fn matrix_add(slots: &mut [String; MAX_MACHINE], name: &str) -> bool {
    if slots.iter().any(|s| is_this_machine(s, name)) {
        return false;
    }
    match slots.iter_mut().find(|s| s.trim().is_empty()) {
        Some(s) => {
            *s = name.trim().to_string();
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// MWB peers

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PeerRow {
    pub name: String,
    pub address: String,
}

pub fn peers_from_map(m: &HashMap<String, String>) -> Vec<PeerRow> {
    let mut v: Vec<PeerRow> = m.iter().map(|(n, a)| PeerRow { name: n.clone(), address: a.clone() }).collect();
    v.sort_by_key(|p| p.name.to_ascii_lowercase());
    v
}

pub fn validate_peer(rows: &[PeerRow], i: usize) -> Option<String> {
    let r = &rows[i];
    let (n, a) = (r.name.trim(), r.address.trim());
    if n.is_empty() && a.is_empty() {
        return None; // blank rows are ignored
    }
    if n.is_empty() {
        return Some("Enter the machine name.".into());
    }
    if a.is_empty() {
        return Some("Enter an address (host, IP or host:port).".into());
    }
    if a.contains(char::is_whitespace) {
        return Some("The address can't contain spaces.".into());
    }
    if rows[..i].iter().any(|o| o.name.trim().eq_ignore_ascii_case(n)) {
        return Some(format!("{n} is listed twice."));
    }
    None
}

pub fn peers_to_map(rows: &[PeerRow]) -> Result<HashMap<String, String>, Vec<String>> {
    let errs: Vec<String> = (0..rows.len()).filter_map(|i| validate_peer(rows, i)).collect();
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(rows
        .iter()
        .filter(|r| !r.name.trim().is_empty())
        .map(|r| (r.name.trim().to_string(), r.address.trim().to_string()))
        .collect())
}

// ---------------------------------------------------------------------------
// Awake

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AwakeKind {
    Off,
    Indefinite,
    Timed,
    Until,
}

impl AwakeKind {
    pub const ALL: [AwakeKind; 4] = [AwakeKind::Off, AwakeKind::Indefinite, AwakeKind::Timed, AwakeKind::Until];
    pub fn label(self) -> &'static str {
        match self {
            AwakeKind::Off => "Off (use the power plan)",
            AwakeKind::Indefinite => "Keep awake indefinitely",
            AwakeKind::Timed => "Keep awake for a time interval",
            AwakeKind::Until => "Keep awake until a time",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AwakeEdit {
    pub kind: AwakeKind,
    pub hours: u32,
    pub minutes: u32,
    /// `YYYY-MM-DD HH:MM` or `HH:MM`.
    pub until: String,
    pub keep_display_on: bool,
    /// Kept from the file (no UI; set by `powermac awake --pid`).
    pub bound_pid: Option<u32>,
}

pub const UNTIL_FORMAT: &str = "%Y-%m-%d %H:%M";

impl AwakeEdit {
    pub fn from_config(c: &awake::AwakeConfig, now: DateTime<Local>) -> Self {
        let mut e = AwakeEdit {
            kind: AwakeKind::Off,
            hours: 1,
            minutes: 0,
            until: (now + chrono::Duration::hours(1)).format(UNTIL_FORMAT).to_string(),
            keep_display_on: c.keep_display_on,
            bound_pid: c.bound_pid,
        };
        match c.mode {
            awake::AwakeMode::Passive => {}
            awake::AwakeMode::Indefinite => e.kind = AwakeKind::Indefinite,
            awake::AwakeMode::Timed(d) => {
                e.kind = AwakeKind::Timed;
                let m = d.as_secs().div_ceil(60);
                e.hours = (m / 60) as u32;
                e.minutes = (m % 60) as u32;
            }
            awake::AwakeMode::Expirable(t) => {
                e.kind = AwakeKind::Until;
                e.until = t.format(UNTIL_FORMAT).to_string();
            }
        }
        e
    }

    /// The config, or why it can't be built.
    pub fn to_config(&self, now: DateTime<Local>) -> Result<awake::AwakeConfig, String> {
        let mode = match self.kind {
            AwakeKind::Off => awake::AwakeMode::Passive,
            AwakeKind::Indefinite => awake::AwakeMode::Indefinite,
            AwakeKind::Timed => {
                let secs = u64::from(self.hours) * 3600 + u64::from(self.minutes) * 60;
                if secs == 0 {
                    return Err("The interval must be longer than 0 minutes.".into());
                }
                awake::AwakeMode::Timed(Duration::from_secs(secs))
            }
            AwakeKind::Until => awake::AwakeMode::Expirable(
                awake::parse_expire_at(&self.until, now)
                    .map_err(|_| "Enter the time as HH:MM or YYYY-MM-DD HH:MM.".to_string())?,
            ),
        };
        Ok(awake::AwakeConfig { mode, keep_display_on: self.keep_display_on, bound_pid: self.bound_pid })
    }

    /// Non-blocking note (e.g. the end time has passed).
    pub fn warning(&self, now: DateTime<Local>) -> Option<String> {
        if self.kind != AwakeKind::Until {
            return None;
        }
        match awake::parse_expire_at(&self.until, now) {
            Ok(t) if t <= now => Some("That time has already passed, so Awake won't keep the computer awake.".into()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Hotkeys (Mouse Utilities use {win, ctrl, alt, shift, code}; Keyboard
// Manager uses its "Ctrl+Shift+V" syntax)

/// Platform names for the modifier families, in display order.
pub fn mod_names() -> [(ModFamily, &'static str); 4] {
    if cfg!(target_os = "macos") {
        [(ModFamily::Ctrl, "Ctrl"), (ModFamily::Alt, "Option"), (ModFamily::Shift, "Shift"), (ModFamily::Win, "Cmd")]
    } else {
        [(ModFamily::Ctrl, "Ctrl"), (ModFamily::Alt, "Alt"), (ModFamily::Shift, "Shift"), (ModFamily::Win, "Win")]
    }
}

/// A recorded key combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Combo {
    pub win: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Windows VK code (0 = none).
    pub vk: u32,
}

impl Combo {
    fn has(&self, f: ModFamily) -> bool {
        match f {
            ModFamily::Win => self.win,
            ModFamily::Ctrl => self.ctrl,
            ModFamily::Alt => self.alt,
            ModFamily::Shift => self.shift,
        }
    }

    /// `"Ctrl+Option+K"` in the key syntax both crates parse.
    pub fn to_text(self) -> String {
        if self.vk == 0 {
            return String::new();
        }
        let mut parts: Vec<String> =
            mod_names().iter().filter(|(f, _)| self.has(*f)).map(|(_, n)| n.to_string()).collect();
        parts.push(key_name(self.vk));
        parts.join("+")
    }
}

pub fn format_hotkey(h: &mouse_utils::Hotkey) -> String {
    Combo { win: h.win, ctrl: h.ctrl, alt: h.alt, shift: h.shift, vk: h.code }.to_text()
}

/// Parse `"Cmd+Shift+H"` (empty or `none` = no hotkey).
pub fn parse_hotkey(s: &str) -> Result<mouse_utils::Hotkey, String> {
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Ok(mouse_utils::Hotkey::none());
    }
    let mut h = mouse_utils::Hotkey::none();
    for vk in parse_combo(t)? {
        match mod_family(vk) {
            Some(ModFamily::Win) => h.win = true,
            Some(ModFamily::Ctrl) => h.ctrl = true,
            Some(ModFamily::Alt) => h.alt = true,
            Some(ModFamily::Shift) => h.shift = true,
            None if h.code != 0 => return Err("Use one key plus modifiers.".into()),
            None if vk > 0xFF => return Err(format!("{} can't be used in a hotkey.", key_name(vk))),
            None => h.code = vk,
        }
    }
    if h.code == 0 {
        return Err("Add a key that isn't a modifier.".into());
    }
    Ok(h)
}

pub fn parse_shortcut(s: &str) -> Result<Shortcut, String> {
    s.parse::<Shortcut>()
}

/// A `{win, ctrl, alt, shift, code}` hotkey (Color Picker, Text Extractor,
/// Window Switcher, Display Control) in the settings key syntax.
pub fn format_hk(win: bool, ctrl: bool, alt: bool, shift: bool, code: u32) -> String {
    Combo { win, ctrl, alt, shift, vk: code }.to_text()
}

/// Parse a `{win, ctrl, alt, shift, code}` hotkey ("" = none).
pub fn parse_hk(s: &str) -> Result<(bool, bool, bool, bool, u32), String> {
    let h = parse_hotkey(s)?;
    Ok((h.win, h.ctrl, h.alt, h.shift, h.code))
}

/// The display-control hotkeys: (label, accessor).
pub const DC_HOTKEYS: [&str; 7] =
    ["Brightness up", "Brightness down", "Contrast up", "Contrast down", "Volume up", "Volume down", "Mute"];

pub fn dc_hotkey_refs(h: &mut display_control::Hotkeys) -> [&mut display_control::Hotkey; 7] {
    [
        &mut h.brightness_up,
        &mut h.brightness_down,
        &mut h.contrast_up,
        &mut h.contrast_down,
        &mut h.volume_up,
        &mut h.volume_down,
        &mut h.mute,
    ]
}

fn dc_hotkey_list(h: &display_control::Hotkeys) -> [display_control::Hotkey; 7] {
    [h.brightness_up, h.brightness_down, h.contrast_up, h.contrast_down, h.volume_up, h.volume_down, h.mute]
}

/// Display name of a smooth-scrolling easing curve.
pub fn easing_label(e: scroll_reverser::Easing) -> &'static str {
    use scroll_reverser::Easing as E;
    match e {
        E::EaseOutCubic => "Smooth (ease out)",
        E::EaseOutQuint => "Snappy",
        E::EaseOutQuad => "Gentle",
        E::EaseInOutCubic => "Ease in and out",
        E::Linear => "Linear",
    }
}

/// Mouse buttons that can be remapped in the settings window.
pub const MAPPABLE_BUTTONS: [(u8, &str); 3] = [
    (mouse_tuning::button::MIDDLE, "Middle button"),
    (mouse_tuning::button::BACK, "Back (side button 4)"),
    (mouse_tuning::button::FORWARD, "Forward (side button 5)"),
];

// ---------------------------------------------------------------------------
// Global hotkey registry: every shortcut a running module listens to, so the
// same keys can't be given to two modules.

/// One shortcut in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyUse {
    pub page: Page,
    pub name: String,
    pub combo: Combo,
}

fn hk_use(page: Page, name: impl Into<String>, win: bool, ctrl: bool, alt: bool, shift: bool, vk: u32) -> HotkeyUse {
    HotkeyUse { page, name: name.into(), combo: Combo { win, ctrl, alt, shift, vk } }
}

fn shortcut_combo(s: &Shortcut) -> Combo {
    use keyboard_manager::keys::Side;
    let on = |x: Side| x != Side::None;
    Combo { win: on(s.mods.win), ctrl: on(s.mods.ctrl), alt: on(s.mods.alt), shift: on(s.mods.shift), vk: s.key }
}

/// The shortcuts of the FancyZones features that are on (`fancyzones.json`).
pub fn fz_hotkey_uses(fz: &fancyzones::Config, enabled: bool) -> Vec<HotkeyUse> {
    if !enabled {
        return Vec::new();
    }
    fz_active_hotkeys(&fz.settings)
        .into_iter()
        .filter(|(_, h)| h.code != 0)
        .map(|(name, h)| {
            let m = h.modifiers;
            hk_use(Page::FancyZones, name, m.meta, m.ctrl, m.alt, m.shift, h.code as u32)
        })
        .collect()
}

/// The shortcuts of the modules in `config.json` that are on.
pub fn hotkey_uses(c: &AppConfig) -> Vec<HotkeyUse> {
    let mut v = Vec::new();
    let mu = &c.mouse_utils;
    let f = &mu.find_my_mouse;
    if f.enabled && f.activation_method == mouse_utils::ActivationMethod::Shortcut {
        let h = f.hotkey;
        v.push(hk_use(Page::MouseUtils, "Find My Mouse", h.win, h.ctrl, h.alt, h.shift, h.code));
    }
    for (on, h, name) in [
        (mu.highlighter.enabled, mu.highlighter.hotkey, "Mouse Highlighter"),
        (mu.crosshairs.enabled, mu.crosshairs.hotkey, "Crosshairs"),
    ] {
        if on {
            v.push(hk_use(Page::MouseUtils, name, h.win, h.ctrl, h.alt, h.shift, h.code));
        }
    }
    let km = &c.keyboard_manager;
    if km.paste_plain_text.enabled {
        v.push(HotkeyUse {
            page: Page::KeyboardManager,
            name: "Paste as plain text".into(),
            combo: shortcut_combo(&km.paste_plain_text.hotkey),
        });
    }
    if km.enabled {
        for (i, r) in km.remap_shortcuts.iter().enumerate() {
            if r.app.is_none() {
                v.push(HotkeyUse {
                    page: Page::KeyboardManager,
                    name: format!("Shortcut remap {}", i + 1),
                    combo: shortcut_combo(&r.from),
                });
            }
        }
    }
    if c.clipboard_history.enabled {
        if let Some(s) = c.clipboard_history.hotkey() {
            v.push(HotkeyUse { page: Page::ClipboardHistory, name: "Clipboard history".into(), combo: shortcut_combo(&s) });
        }
    }
    if c.color_picker.enabled {
        let h = c.color_picker.settings.hotkey;
        v.push(hk_use(Page::ColorPicker, "Color Picker", h.win, h.ctrl, h.alt, h.shift, h.code));
    }
    if c.text_extractor.enabled {
        let h = c.text_extractor.settings.hotkey;
        v.push(hk_use(Page::TextExtractor, "Text Extractor", h.win, h.ctrl, h.alt, h.shift, h.code));
    }
    let ws = &c.window_switcher;
    if ws.enabled {
        for (h, name) in [(ws.trigger, "Switch between windows"), (ws.app_windows_trigger, "Switch between an app's windows")] {
            v.push(hk_use(Page::WindowSwitcher, name, h.win, h.ctrl, h.alt, h.shift, h.code));
        }
        if ws.replace_cmd_tab {
            let h = window_switcher::Hotkey::system_switcher();
            v.push(hk_use(Page::WindowSwitcher, "Replaced system switcher", h.win, h.ctrl, h.alt, h.shift, h.code));
        }
    }
    let dc = &c.display_control;
    if dc.enabled {
        for (h, name) in dc_hotkey_list(&dc.hotkeys).into_iter().zip(DC_HOTKEYS) {
            v.push(hk_use(Page::Displays, name, h.win, h.ctrl, h.alt, h.shift, h.code));
        }
    }
    let ds = &c.drag_shelf;
    if ds.enabled && drag_shelf::SUPPORTED && ds.hotkey.is_set() {
        let h = ds.hotkey;
        v.push(hk_use(Page::DragShelf, "New drag shelf", h.win, h.ctrl, h.alt, h.shift, h.code));
    }
    let wsp = &c.workspaces;
    if wsp.enabled && wsp.hotkey.is_set() {
        let h = wsp.hotkey;
        v.push(hk_use(Page::Workspaces, "Workspaces editor", h.win, h.ctrl, h.alt, h.shift, h.code));
    }
    if km.enabled {
        for (i, h) in km.hotkeys.iter().enumerate() {
            if !h.enabled || h.app.is_some() {
                continue;
            }
            let combo = match h.from {
                keyboard_manager::Trigger::Key(k) => Combo { vk: k, ..Default::default() },
                keyboard_manager::Trigger::Shortcut(s) => shortcut_combo(&s),
            };
            v.push(HotkeyUse { page: Page::Automation, name: format!("Hotkey {} ({})", i + 1, h.action.label()), combo });
        }
    }
    v.retain(|u| u.combo.vk != 0);
    v
}

/// One automation hotkey being edited: the trigger as typed / recorded.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HotkeyRow {
    /// "F13", "Ctrl+Alt+B".
    pub from: String,
    pub action: keyboard_manager::automation::Action,
    /// Only while this app is in front (shortcuts only); empty = every app.
    pub app: String,
    pub enabled: bool,
}

impl HotkeyRow {
    pub fn new() -> Self {
        HotkeyRow {
            from: String::new(),
            action: keyboard_manager::automation::Action::Shell(Default::default()),
            app: String::new(),
            enabled: true,
        }
    }

    pub fn from_hotkey(h: &keyboard_manager::Hotkey) -> Self {
        HotkeyRow { from: h.from.to_string(), action: h.action.clone(), app: h.app.clone().unwrap_or_default(), enabled: h.enabled }
    }

    /// Nothing recorded yet (not saved).
    pub fn is_blank(&self) -> bool {
        self.from.trim().is_empty()
    }

    pub fn hotkey(&self) -> Result<keyboard_manager::Hotkey, String> {
        let from: keyboard_manager::Trigger = self.from.trim().parse()?;
        let app = self.app.trim();
        if !app.is_empty() && matches!(from, keyboard_manager::Trigger::Key(_)) {
            return Err(format!("{from} is a single key; only shortcuts can be limited to one app."));
        }
        if let keyboard_manager::automation::Action::Shortcut { keys } = &self.action {
            parse_combo(keys).map_err(|e| format!("the shortcut to press: {e}"))?;
        }
        Ok(keyboard_manager::Hotkey {
            from,
            action: self.action.clone(),
            app: (!app.is_empty()).then(|| app.to_string()),
            enabled: self.enabled,
        })
    }
}

impl Default for HotkeyRow {
    fn default() -> Self {
        Self::new()
    }
}

/// Shortcuts used twice. Each clash is reported once, on the page of the
/// later entry (so put the entries of other files first).
pub fn hotkey_conflicts(uses: &[HotkeyUse]) -> Vec<Problem> {
    let mut out = Vec::new();
    for (i, u) in uses.iter().enumerate() {
        if u.combo.vk == 0 {
            continue;
        }
        if let Some(o) = uses[..i].iter().find(|o| o.combo == u.combo) {
            if o.page == Page::FancyZones && u.page == Page::FancyZones {
                continue; // validate_fancyzones reports these
            }
            let where_ = if o.page == u.page { String::new() } else { format!(" ({})", o.page.title()) };
            let what = if u.name == u.page.title() { "Shortcut".to_string() } else { u.name.clone() };
            out.push(problem(
                u.page,
                format!("{what}: {} is already used by {}{where_}.", u.combo.to_text(), o.name),
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Keyboard Manager rows

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TargetKind {
    Keys,
    Disabled,
    Text,
    OpenUri,
    Run,
    PastePlainText,
}

impl TargetKind {
    pub const ALL: [TargetKind; 6] = [
        TargetKind::Keys,
        TargetKind::Disabled,
        TargetKind::Text,
        TargetKind::OpenUri,
        TargetKind::Run,
        TargetKind::PastePlainText,
    ];
    pub fn label(self) -> &'static str {
        match self {
            TargetKind::Keys => "Key / shortcut",
            TargetKind::Disabled => "Disabled",
            TargetKind::Text => "Type text",
            TargetKind::OpenUri => "Open URL / file",
            TargetKind::Run => "Run program",
            TargetKind::PastePlainText => "Paste as plain text",
        }
    }
}

/// One row of the key or shortcut remap table.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RemapRow {
    /// A key name (key remaps) or a shortcut (shortcut remaps).
    pub from: String,
    pub kind: TargetKind,
    /// Keys, text, URI or program path, depending on `kind`.
    pub value: String,
    /// Program arguments, separated by spaces.
    pub args: String,
    /// Kept from the file (no UI).
    pub working_dir: Option<String>,
    /// Shortcut remaps only: limit to this app.
    pub app: String,
}

impl RemapRow {
    pub fn new() -> Self {
        RemapRow {
            from: String::new(),
            kind: TargetKind::Keys,
            value: String::new(),
            args: String::new(),
            working_dir: None,
            app: String::new(),
        }
    }

    fn with_target(from: String, t: &Target, app: Option<&str>) -> Self {
        let mut r = RemapRow { from, app: app.unwrap_or_default().to_string(), ..RemapRow::new() };
        match t {
            Target::Disabled => r.kind = TargetKind::Disabled,
            Target::Key(k) => r.value = key_name(*k),
            Target::Shortcut(s) => r.value = s.to_string(),
            Target::Text(s) => {
                r.kind = TargetKind::Text;
                r.value = s.clone();
            }
            Target::OpenUri(u) => {
                r.kind = TargetKind::OpenUri;
                r.value = u.clone();
            }
            Target::Run(p) => {
                r.kind = TargetKind::Run;
                r.value = p.path.clone();
                r.args = p.args.join(" ");
                r.working_dir = p.working_dir.clone();
            }
            Target::PastePlainText => r.kind = TargetKind::PastePlainText,
        }
        r
    }

    pub fn from_key_remap(k: &KeyRemap) -> Self {
        Self::with_target(key_name(k.from.0), &k.to, None)
    }

    pub fn from_shortcut_remap(s: &ShortcutRemap) -> Self {
        Self::with_target(s.from.to_string(), &s.to, s.app.as_deref())
    }

    pub fn is_blank(&self) -> bool {
        self.from.trim().is_empty() && self.value.trim().is_empty()
    }

    pub fn parse_from_key(&self) -> Result<Key, String> {
        let t = self.from.trim();
        if t.is_empty() {
            return Err("Enter the key to remap.".into());
        }
        parse_key(t).map(Key).ok_or_else(|| format!("Unknown key name '{t}'."))
    }

    pub fn parse_from_shortcut(&self) -> Result<Shortcut, String> {
        if self.from.trim().is_empty() {
            return Err("Enter the shortcut to remap.".into());
        }
        parse_shortcut(&self.from)
    }

    pub fn parse_target(&self) -> Result<Target, String> {
        let v = self.value.trim();
        match self.kind {
            TargetKind::Keys if v.is_empty() => Err("Enter the key or shortcut to send.".into()),
            TargetKind::Keys => match Target::parse(v)? {
                Target::Key(_) | Target::Shortcut(_) => Target::parse(v),
                _ => Err("Pick the matching action instead of typing it.".into()),
            },
            TargetKind::Disabled => Ok(Target::Disabled),
            TargetKind::Text if self.value.is_empty() => Err("Enter the text to type.".into()),
            TargetKind::Text => Ok(Target::Text(self.value.clone())),
            TargetKind::OpenUri if v.is_empty() => Err("Enter a URL, file or folder.".into()),
            TargetKind::OpenUri => Ok(Target::OpenUri(v.to_string())),
            TargetKind::Run if v.is_empty() => Err("Enter the program's path.".into()),
            TargetKind::Run => Ok(Target::Run(RunProgram {
                path: v.to_string(),
                args: self.args.split_whitespace().map(str::to_string).collect(),
                working_dir: self.working_dir.clone(),
            })),
            TargetKind::PastePlainText => Ok(Target::PastePlainText),
        }
    }

    /// First problem with this row as a key remap.
    pub fn key_remap(&self) -> Result<KeyRemap, String> {
        Ok(KeyRemap { from: self.parse_from_key()?, to: self.parse_target()? })
    }

    pub fn shortcut_remap(&self) -> Result<ShortcutRemap, String> {
        let app = self.app.trim();
        Ok(ShortcutRemap {
            from: self.parse_from_shortcut()?,
            to: self.parse_target()?,
            app: (!app.is_empty()).then(|| app.to_string()),
        })
    }
}

impl Default for RemapRow {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// The draft of config.json

/// Everything the window edits in `config.json`. Simple fields are edited
/// in `cfg` directly; the rest live in buffers until they validate.
#[derive(Debug, Clone, Serialize)]
pub struct AppDraft {
    pub cfg: AppConfig,
    pub matrix: [String; MAX_MACHINE],
    pub peers: Vec<PeerRow>,
    pub awake: AwakeEdit,
    pub find_hotkey: String,
    pub highlighter_hotkey: String,
    pub crosshairs_hotkey: String,
    pub key_remaps: Vec<RemapRow>,
    pub shortcut_remaps: Vec<RemapRow>,
    pub paste_hotkey: String,
    pub color_hotkey: String,
    pub ocr_hotkey: String,
    pub switch_trigger: String,
    pub switch_app_trigger: String,
    pub display_hotkeys: [String; 7],
    /// Workspaces: the shortcut that opens the editor.
    pub workspaces_hotkey: String,
    /// Drag Shelf: the shortcut that opens an empty shelf.
    pub drag_shelf_hotkey: String,
    /// Automation hotkeys (`keyboardManager.hotkeys`), with the trigger as text.
    pub hotkeys: Vec<HotkeyRow>,
    /// Shortcuts of other files (FancyZones), for the conflict check.
    #[serde(skip)]
    pub external_hotkeys: Vec<HotkeyUse>,
}

impl AppDraft {
    pub fn new(cfg: AppConfig, now: DateTime<Local>) -> Self {
        let m = &cfg.mouse_utils;
        let k = &cfg.keyboard_manager;
        AppDraft {
            matrix: matrix_from_vec(&cfg.mwb.settings.matrix),
            peers: peers_from_map(&cfg.mwb.settings.peers),
            awake: AwakeEdit::from_config(&cfg.awake.settings, now),
            find_hotkey: format_hotkey(&m.find_my_mouse.hotkey),
            highlighter_hotkey: format_hotkey(&m.highlighter.hotkey),
            crosshairs_hotkey: format_hotkey(&m.crosshairs.hotkey),
            key_remaps: k.remap_keys.iter().map(RemapRow::from_key_remap).collect(),
            shortcut_remaps: k.remap_shortcuts.iter().map(RemapRow::from_shortcut_remap).collect(),
            paste_hotkey: k.paste_plain_text.hotkey.to_string(),
            color_hotkey: {
                let h = cfg.color_picker.settings.hotkey;
                format_hk(h.win, h.ctrl, h.alt, h.shift, h.code)
            },
            ocr_hotkey: {
                let h = cfg.text_extractor.settings.hotkey;
                format_hk(h.win, h.ctrl, h.alt, h.shift, h.code)
            },
            switch_trigger: {
                let h = cfg.window_switcher.trigger;
                format_hk(h.win, h.ctrl, h.alt, h.shift, h.code)
            },
            switch_app_trigger: {
                let h = cfg.window_switcher.app_windows_trigger;
                format_hk(h.win, h.ctrl, h.alt, h.shift, h.code)
            },
            display_hotkeys: dc_hotkey_list(&cfg.display_control.hotkeys)
                .map(|h| format_hk(h.win, h.ctrl, h.alt, h.shift, h.code)),
            workspaces_hotkey: {
                let h = cfg.workspaces.hotkey;
                format_hk(h.win, h.ctrl, h.alt, h.shift, h.code)
            },
            drag_shelf_hotkey: {
                let h = cfg.drag_shelf.hotkey;
                format_hk(h.win, h.ctrl, h.alt, h.shift, h.code)
            },
            hotkeys: k.hotkeys.iter().map(HotkeyRow::from_hotkey).collect(),
            external_hotkeys: Vec::new(),
            cfg,
        }
    }

    /// This machine's name as MWB announces it.
    pub fn this_machine(&self) -> String {
        self.cfg.mwb.settings.effective_machine_name()
    }

    /// The config to save, or everything that's wrong with the draft.
    pub fn build(&self, now: DateTime<Local>) -> Result<AppConfig, Vec<Problem>> {
        let mut c = self.cfg.clone();
        let mut errs = Vec::new();

        // Mouse Without Borders
        let mwb = &mut c.mwb.settings;
        mwb.key = mwb::normalize_key(&mwb.key);
        if c.mwb.enabled {
            if let Some(e) = validate_key(&mwb.key) {
                errs.push(problem(Page::Mwb, e));
            }
        }
        mwb.machine_name = mwb.machine_name.trim().to_string();
        if let Some(e) = validate_machine_name(&mwb.machine_name) {
            errs.push(problem(Page::Mwb, e));
        }
        errs.extend(validate_matrix(&self.matrix).into_iter().map(|e| problem(Page::Mwb, e)));
        mwb.matrix = matrix_to_vec(&self.matrix);
        match peers_to_map(&self.peers) {
            Ok(p) => mwb.peers = p,
            Err(e) => errs.extend(e.into_iter().map(|e| problem(Page::Mwb, format!("Peers: {e}")))),
        }
        if mwb.port == 0 || mwb.port == u16::MAX {
            errs.push(problem(Page::Mwb, "The port must be between 1 and 65534."));
        }

        // Awake
        match self.awake.to_config(now) {
            Ok(a) => c.awake.settings = a,
            Err(e) => errs.push(problem(Page::Awake, e)),
        }

        // Mouse Utilities
        let mu = &mut c.mouse_utils;
        for (text, hk, name) in [
            (&self.find_hotkey, &mut mu.find_my_mouse.hotkey, "Find My Mouse"),
            (&self.highlighter_hotkey, &mut mu.highlighter.hotkey, "Mouse Highlighter"),
            (&self.crosshairs_hotkey, &mut mu.crosshairs.hotkey, "Crosshairs"),
        ] {
            match parse_hotkey(text) {
                Ok(h) => *hk = h,
                Err(e) => errs.push(problem(Page::MouseUtils, format!("{name} shortcut: {e}"))),
            }
        }

        // Keyboard Manager
        let km = &mut c.keyboard_manager;
        km.remap_keys.clear();
        for (i, r) in self.key_remaps.iter().enumerate().filter(|(_, r)| !r.is_blank()) {
            match r.key_remap() {
                Ok(k) => km.remap_keys.push(k),
                Err(e) => errs.push(problem(Page::KeyboardManager, format!("Key remap {}: {e}", i + 1))),
            }
        }
        km.remap_shortcuts.clear();
        for (i, r) in self.shortcut_remaps.iter().enumerate().filter(|(_, r)| !r.is_blank()) {
            match r.shortcut_remap() {
                Ok(s) => km.remap_shortcuts.push(s),
                Err(e) => errs.push(problem(Page::KeyboardManager, format!("Shortcut remap {}: {e}", i + 1))),
            }
        }
        match parse_shortcut(&self.paste_hotkey) {
            Ok(s) => km.paste_plain_text.hotkey = s,
            Err(e) => errs.push(problem(Page::KeyboardManager, format!("Paste as plain text shortcut: {e}"))),
        }
        km.hotkeys.clear();
        for (i, h) in self.hotkeys.iter().enumerate() {
            if h.is_blank() {
                continue;
            }
            match h.hotkey() {
                Ok(hk) => km.hotkeys.push(hk),
                Err(e) => errs.push(problem(Page::Automation, format!("Hotkey {}: {e}", i + 1))),
            }
        }
        if km.windows_style.excluded_apps.iter().any(|a| a.trim().is_empty()) {
            errs.push(problem(Page::KeyboardManager, "Windows-style keys, excluded apps: remove the empty entry."));
        }

        // Mouse
        for m in &c.mouse_tuning.buttons.mappings {
            if let mouse_tuning::ButtonAction::Action(keyboard_manager::automation::Action::Shortcut { keys }) = &m.action {
                if let Err(e) = keyboard_manager::keys::parse_combo(keys) {
                    let name = MAPPABLE_BUTTONS.iter().find(|(b, _)| *b == m.button).map(|(_, n)| *n).unwrap_or("Button");
                    errs.push(problem(Page::Automation, format!("{name}: {e}")));
                }
            }
            if let mouse_tuning::ButtonAction::Keystroke { keys } = &m.action {
                if let Err(e) = mouse_tuning::keys::parse_combo(keys) {
                    let name = MAPPABLE_BUTTONS.iter().find(|(b, _)| *b == m.button).map(|(_, n)| *n).unwrap_or("Button");
                    errs.push(problem(Page::MouseTuning, format!("{name}: {e}")));
                }
            }
        }

        // Displays
        for ((text, hk), name) in self.display_hotkeys.iter().zip(dc_hotkey_refs(&mut c.display_control.hotkeys)).zip(DC_HOTKEYS) {
            match parse_hk(text) {
                Ok((w, ct, a, sh, code)) => *hk = display_control::Hotkey::new(w, ct, a, sh, code),
                Err(e) => errs.push(problem(Page::Displays, format!("{name} shortcut: {e}"))),
            }
        }

        // Clipboard History
        let ch = &mut c.clipboard_history;
        ch.hotkey = ch.hotkey.trim().to_string();
        if !ch.hotkey.is_empty() {
            if let Err(e) = parse_shortcut(&ch.hotkey) {
                errs.push(problem(Page::ClipboardHistory, format!("Shortcut: {e}")));
            }
        }
        if ch.excluded_apps.iter().any(|a| a.trim().is_empty()) {
            errs.push(problem(Page::ClipboardHistory, "Excluded apps: remove the empty entry."));
        }

        // Color Picker
        match parse_hk(&self.color_hotkey) {
            Ok((w, ct, a, sh, code)) => c.color_picker.settings.hotkey = color_picker::Hotkey::new(w, ct, a, sh, code),
            Err(e) => errs.push(problem(Page::ColorPicker, format!("Shortcut: {e}"))),
        }
        errs.extend(validate_color_formats(&c.color_picker.settings).into_iter().map(|e| problem(Page::ColorPicker, e)));

        // Workspaces
        match parse_hk(&self.workspaces_hotkey) {
            Ok((w, ct, a, sh, code)) => c.workspaces.hotkey = workspaces::Hotkey::new(w, ct, a, sh, code),
            Err(e) => errs.push(problem(Page::Workspaces, format!("Shortcut: {e}"))),
        }
        if c.workspaces.capture.excluded_apps.iter().any(|a| a.trim().is_empty()) {
            errs.push(problem(Page::Workspaces, "Excluded apps: remove the empty entry."));
        }

        // Drag Shelf
        match parse_hk(&self.drag_shelf_hotkey) {
            Ok((w, ct, a, sh, code)) => c.drag_shelf.hotkey = drag_shelf::Hotkey::new(w, ct, a, sh, code),
            Err(e) => errs.push(problem(Page::DragShelf, format!("Shortcut: {e}"))),
        }
        let ds = &mut c.drag_shelf;
        ds.max_shelves = ds.max_shelves.clamp(1, drag_shelf::model::MAX_SHELVES as u32);
        ds.auto_close_delay_ms = ds.auto_close_delay_ms.min(60_000);
        ds.custom_shake = ds.custom_shake.clamped();

        // Text Extractor
        match parse_hk(&self.ocr_hotkey) {
            Ok((w, ct, a, sh, code)) => c.text_extractor.settings.hotkey = text_extractor::Hotkey::new(w, ct, a, sh, code),
            Err(e) => errs.push(problem(Page::TextExtractor, format!("Shortcut: {e}"))),
        }

        // Window Switcher
        let ws = &mut c.window_switcher;
        for (text, hk, name) in [
            (&self.switch_trigger, &mut ws.trigger, "Switch between windows"),
            (&self.switch_app_trigger, &mut ws.app_windows_trigger, "Switch between an app's windows"),
        ] {
            match parse_hk(text) {
                Ok((false, false, false, _, code)) if code != 0 => {
                    errs.push(problem(Page::WindowSwitcher, format!("{name}: add a modifier to hold (Option, Ctrl or Cmd).")))
                }
                Ok((w, ct, a, sh, code)) => *hk = window_switcher::Hotkey::new(w, ct, a, sh, code),
                Err(e) => errs.push(problem(Page::WindowSwitcher, format!("{name}: {e}"))),
            }
        }
        if ws.excluded_apps.iter().any(|a| a.trim().is_empty()) {
            errs.push(problem(Page::WindowSwitcher, "Excluded apps: remove the empty entry."));
        }

        // The same shortcut in two places (only checked once each field parses).
        if errs.is_empty() {
            let mut uses = self.external_hotkeys.clone();
            uses.extend(hotkey_uses(&c));
            errs.extend(hotkey_conflicts(&uses));
        }

        if errs.is_empty() {
            Ok(c)
        } else {
            Err(errs)
        }
    }
}

/// Problems with the Color Picker formats.
pub fn validate_color_formats(c: &color_picker::Config) -> Vec<String> {
    let mut out = Vec::new();
    for (i, f) in c.custom_formats.iter().enumerate() {
        let n = f.name.trim();
        if n.is_empty() {
            out.push(format!("Custom format {}: give it a name.", i + 1));
        } else if c.custom_formats[..i].iter().any(|o| o.name.trim() == n) {
            out.push(format!("There are two custom formats named “{n}”."));
        }
        if f.format.trim().is_empty() {
            out.push(format!("Custom format “{n}”: enter a template, like #%ReX%GrX%BlX."));
        }
    }
    if c.template(&c.copied_format).is_none() {
        out.push(format!("The copied format “{}” doesn't exist.", c.copied_format));
    }
    out
}

// ---------------------------------------------------------------------------
// file-tools.json

/// The parts of `file-tools.json` the settings window edits (PowerRename
/// keeps its recently used terms and options there too).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FtEdit {
    pub remember_options: bool,
    pub show_only_changed: bool,
    pub mru_size: usize,
    pub new_plus: file_tools::NewPlusConfig,
    /// Image Resizer presets and defaults (its window keeps the last
    /// choices here too).
    pub resize: file_tools::ResizeConfig,
    /// Finder basics: terminal / editor, copy path, eject.
    pub finder: file_tools::FinderConfig,
}

impl FtEdit {
    pub fn of(c: &file_tools::Config) -> Self {
        FtEdit {
            remember_options: c.rename.remember_options,
            show_only_changed: c.rename.show_only_changed,
            mru_size: c.rename.mru_size,
            new_plus: c.new_plus.clone(),
            resize: c.resize.clone(),
            finder: c.finder.clone(),
        }
    }
}

/// Problems with the Image Resizer presets (they block saving file-tools.json).
pub fn validate_resize(r: &file_tools::ResizeConfig) -> Vec<String> {
    let mut out = Vec::new();
    for (i, p) in r.presets.iter().enumerate() {
        let n = p.name.trim();
        if n.is_empty() {
            out.push(format!("Size {}: give it a name.", i + 1));
        } else if r.presets[..i].iter().any(|o| o.name.trim().eq_ignore_ascii_case(n)) {
            out.push(format!("There are two sizes named “{n}”."));
        }
        if !(p.width >= 0.0 && p.height >= 0.0) || (p.width == 0.0 && p.height == 0.0) {
            out.push(format!("Size “{n}”: enter a width or a height (0 = automatic)."));
        }
    }
    if !(1..=100).contains(&r.options.jpeg_quality) {
        out.push("Quality must be 1–100.".into());
    }
    if r.options.file_name_format.trim().is_empty() && !r.options.replace {
        out.push("Enter a file name pattern, like %1 (%2).".into());
    }
    out
}

/// Write the edited parts into `path`, keeping the rest as it is on disk now.
pub fn save_file_tools(path: &Path, e: &FtEdit) -> Result<()> {
    let mut c = file_tools::Config::load_from(path);
    c.rename.remember_options = e.remember_options;
    c.rename.show_only_changed = e.show_only_changed;
    c.rename.mru_size = e.mru_size;
    c.new_plus = e.new_plus.clone();
    c.resize = e.resize.clone();
    c.finder = e.finder.clone();
    c.save_to(path)
}

// ---------------------------------------------------------------------------
// fancyzones.json

/// The FancyZones config next to `config.json`.
pub fn fancyzones_path(config_path: &Path) -> std::path::PathBuf {
    config_path.with_file_name("fancyzones.json")
}

pub fn load_fancyzones(path: &Path) -> Result<fancyzones::Config> {
    if path.exists() {
        fancyzones::Config::load_from(path)
    } else {
        Ok(fancyzones::Config::default())
    }
}

/// Ask the running PowerMac to flash the zones (see
/// [`fancyzones::FLASH_REQUEST_FILE`]).
pub fn request_zone_flash(config_path: &Path, monitor: Option<String>) -> Result<()> {
    let path = config_path.with_file_name(fancyzones::FLASH_REQUEST_FILE);
    let json = serde_json::to_string(&fancyzones::FlashRequest::now(monitor))?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
}

/// The parts of `fancyzones.json` this window edits.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FzEdit {
    pub settings: fancyzones::Settings,
    pub applied_layouts: std::collections::BTreeMap<String, fancyzones::AppliedLayout>,
}

impl FzEdit {
    pub fn of(c: &fancyzones::Config) -> Self {
        FzEdit { settings: c.settings.clone(), applied_layouts: c.applied_layouts.clone() }
    }
}

/// Write the edited parts into `path`, keeping what the running FancyZones
/// keeps there itself (window history, custom layouts) as it is on disk now.
///
/// A monitor whose layout changed here gets it on every Space: its per-Space
/// copies (`virtual_desktop_layouts`, used first when layouts per Space are
/// on) are dropped, or they would keep overriding what was picked here.
pub fn save_fancyzones(path: &Path, edit: &FzEdit) -> Result<()> {
    let mut c = load_fancyzones(path).unwrap_or_default();
    let changed: Vec<String> = c
        .applied_layouts
        .keys()
        .chain(edit.applied_layouts.keys())
        .filter(|id| c.applied_layouts.get(*id) != edit.applied_layouts.get(*id))
        .cloned()
        .collect();
    for per_space in c.virtual_desktop_layouts.values_mut() {
        for id in &changed {
            per_space.remove(id);
        }
    }
    c.virtual_desktop_layouts.retain(|_, m| !m.is_empty());
    c.settings = edit.settings.clone();
    c.applied_layouts = edit.applied_layouts.clone();
    c.save_to(path)
}

/// Problems with a layout (e.g. too many zones for a grid), checked by
/// instantiating it on a 1920x1080 work area.
pub fn validate_layout(l: &fancyzones::AppliedLayout, custom: &[fancyzones::CustomLayout]) -> Option<String> {
    if l.kind != fancyzones::LayoutType::Blank && l.kind != fancyzones::LayoutType::Custom && l.zone_count < 1 {
        return Some("A layout needs at least one zone.".into());
    }
    fancyzones::Layout::new(l, fancyzones::Rect::new(0, 0, 1920, 1080), custom).err().map(|e| e.to_string())
}

/// A FancyZones hotkey in the settings window's key syntax ("" = none).
pub fn format_fz_hotkey(h: &fancyzones::Hotkey) -> String {
    let m = h.modifiers;
    Combo { win: m.meta, ctrl: m.ctrl, alt: m.alt, shift: m.shift, vk: h.code as u32 }.to_text()
}

/// Parse a FancyZones hotkey: the settings syntax (what Record produces),
/// or anything `fancyzones::Hotkey::from_str` accepts. Empty = none (code 0).
pub fn parse_fz_hotkey(s: &str) -> Result<fancyzones::Hotkey, String> {
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Ok(fancyzones::Hotkey::default());
    }
    match parse_hotkey(t) {
        Ok(h) => Ok(fancyzones::Hotkey::new(fancyzones::Modifiers::new(h.ctrl, h.alt, h.shift, h.win), h.code as u16)),
        Err(e) => t.parse::<fancyzones::Hotkey>().map_err(|_| e),
    }
}

/// The FancyZones hotkeys that are switched on, with their labels.
pub fn fz_active_hotkeys(s: &fancyzones::Settings) -> Vec<(&'static str, fancyzones::Hotkey)> {
    let mut v = vec![("Layout editor", s.editor_hotkey)];
    if s.window_switching {
        v.push(("Next window in zone", s.next_tab_hotkey));
        v.push(("Previous window in zone", s.prev_tab_hotkey));
    }
    if s.monitor_rotation {
        v.push(("Monitor rotation", s.monitor_rotation_hotkey));
    }
    v
}

pub fn validate_fancyzones(c: &fancyzones::Config) -> Vec<Problem> {
    let mut out = Vec::new();
    let s = &c.settings;
    for (name, color) in [
        ("Zone color", &s.zone_color),
        ("Border color", &s.zone_border_color),
        ("Highlight color", &s.zone_highlight_color),
        ("Number color", &s.zone_number_color),
        ("Layout name text color", &s.layout_name_label_text_color),
        ("Layout name background", &s.layout_name_label_background_color),
    ] {
        if fancyzones::config::parse_hex_color(color).is_none() {
            out.push(problem(Page::FancyZones, format!("{name} “{color}” isn't a #RRGGBB color.")));
        }
    }
    let hks = fz_active_hotkeys(s);
    for (i, (name, h)) in hks.iter().enumerate() {
        let required = *name != "Layout editor";
        if h.code == 0 && required {
            out.push(problem(Page::FancyZones, format!("{name}: pick a shortcut or turn the feature off.")));
        }
        if h.code != 0 && h.modifiers == fancyzones::Modifiers::default() {
            out.push(problem(Page::FancyZones, format!("{name}: add at least one modifier.")));
        }
        if let Some((other, _)) = hks[..i].iter().find(|(_, o)| o.code != 0 && o == h) {
            out.push(problem(Page::FancyZones, format!("{name} uses the same shortcut as {other}.")));
        }
    }
    if s.excluded_apps.iter().any(|a| a.trim().is_empty()) {
        out.push(problem(Page::FancyZones, "Excluded apps: remove the empty entry."));
    }
    if s.layout_name_label_font_size < 6 || s.layout_name_label_font_size > 200 {
        out.push(problem(Page::FancyZones, "Layout name font size must be 6–200.".to_string()));
    }
    if let Some(e) = validate_layout(&c.settings.default_layout, &c.custom_layouts) {
        out.push(problem(Page::FancyZones, format!("Default layout: {e}")));
    }
    for (id, l) in &c.applied_layouts {
        if let Some(e) = validate_layout(l, &c.custom_layouts) {
            out.push(problem(Page::FancyZones, format!("Layout for {id}: {e}")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn fancyzones_layout_picked_in_settings_beats_per_space_copy() {
        use fancyzones::{AppliedLayout, LayoutType};
        let dir = std::env::temp_dir().join(format!("pm-fz-vd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fancyzones.json");
        let grid = AppliedLayout::template(LayoutType::PriorityGrid, 3);
        let cols = AppliedLayout::template(LayoutType::Columns, 2);
        let mut disk = fancyzones::Config::default();
        disk.settings.layouts_per_virtual_desktop = true;
        for id in ["wide", "laptop"] {
            disk.set_layout(id, Some("space-1"), grid.clone());
        }
        disk.save_to(&path).unwrap();

        let mut edit = FzEdit::of(&disk);
        edit.applied_layouts.insert("wide".into(), cols.clone());
        save_fancyzones(&path, &edit).unwrap();

        let c = fancyzones::Config::load_from(&path).unwrap();
        assert_eq!(c.layout_for_desktop("wide", Some("space-1")), cols, "Settings' pick is used on the Space");
        assert_eq!(c.layout_for_desktop("laptop", Some("space-1")), grid, "other monitors keep theirs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn page_names() {
        use super::Page;
        assert_eq!(Page::NotchWidgets.slug(), "notch-widgets");
        assert_eq!(Page::Mwb.slug(), "mouse-without-borders");
        for n in ["notch-widgets", "Notch Widgets", "NotchWidgets", "notchwidgets", " notch_widgets "] {
            assert_eq!(Page::from_name(n), Some(Page::NotchWidgets), "{n}");
        }
        assert_eq!(Page::from_name("mwb"), Some(Page::Mwb));
        assert_eq!(Page::from_name("sound"), Some(Page::Sound));
        for (n, p) in [("mixer", Page::Mixer), ("battery", Page::Battery), ("automation", Page::Automation), ("network", Page::Network)] {
            assert_eq!(Page::from_name(n), Some(p));
            assert!(p.section().is_some());
        }
        assert_eq!(Page::Network.section(), Some("Security & Network"));
        assert_eq!(Page::from_name(""), None);
        assert_eq!(Page::from_name("teleporter"), None);
        for p in Page::ALL {
            assert_eq!(Page::from_name(&p.slug()), Some(p));
        }
    }

    #[test]
    fn matrix_drag_moves() {
        let mut m: [String; super::MAX_MACHINE] = ["a".into(), "b".into(), "c".into(), "".into()];
        super::matrix_move(&mut m, 0, 2);
        assert_eq!(m, ["b", "c", "a", ""]);
        super::matrix_move(&mut m, 3, 0);
        assert_eq!(m, ["", "b", "c", "a"]);
        super::matrix_move(&mut m, 1, 1);
        assert_eq!(m, ["", "b", "c", "a"]);
    }

    #[test]
    fn flash_request_file() {
        let d = std::env::temp_dir().join(format!("pm-flash-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let cfg = d.join("config.json");
        super::request_zone_flash(&cfg, Some("mon".into())).unwrap();
        let r: fancyzones::FlashRequest = serde_json::from_str(
            &std::fs::read_to_string(d.join(fancyzones::FLASH_REQUEST_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(r.monitor.as_deref(), Some("mon"));
        assert!(r.is_fresh(5_000));
        std::fs::remove_dir_all(&d).ok();
    }

    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap()
    }

    fn slots(a: [&str; 4]) -> [String; 4] {
        a.map(String::from)
    }

    #[test]
    fn matrix_reordering() {
        let mut s = slots(["a", "b", "c", ""]);
        matrix_move_up(&mut s, 0); // no-op
        assert_eq!(s, slots(["a", "b", "c", ""]));
        matrix_move_up(&mut s, 2);
        assert_eq!(s, slots(["a", "c", "b", ""]));
        matrix_move_down(&mut s, 0);
        assert_eq!(s, slots(["c", "a", "b", ""]));
        matrix_move_down(&mut s, 3); // no-op
        matrix_move_down(&mut s, 2);
        assert_eq!(s, slots(["c", "a", "", "b"]));
        // inner gaps survive (2x2 position), trailing ones don't
        assert_eq!(matrix_to_vec(&s), ["c", "a", "", "b"]);
        assert_eq!(matrix_to_vec(&slots([" x ", "", "", ""])), ["x"]);
        assert_eq!(matrix_from_vec(&["a".into(), " b".into()]), slots(["a", "b", "", ""]));
        assert_eq!(matrix_from_vec(&(0..6).map(|i| i.to_string()).collect::<Vec<_>>())[3], "3");
    }

    #[test]
    fn matrix_validation_and_add() {
        assert!(validate_matrix(&slots(["mac", "pc", "", ""])).is_empty());
        assert_eq!(validate_matrix(&slots(["mac", "MAC", "", ""])).len(), 1);
        assert_eq!(validate_matrix(&slots(["mac", &"x".repeat(33), "", ""])).len(), 1);
        let mut s = slots(["", "pc", "", ""]);
        assert!(matrix_add(&mut s, "mac"));
        assert_eq!(s, slots(["mac", "pc", "", ""]));
        assert!(!matrix_add(&mut s, "MAC"));
        assert!(is_this_machine(" Mac ", "mac"));
        assert!(!is_this_machine("", ""));
    }

    #[test]
    fn peers() {
        let rows = vec![
            PeerRow { name: "pc".into(), address: "10.0.0.2".into() },
            PeerRow::default(),
            PeerRow { name: " laptop ".into(), address: " laptop.lan:15100 ".into() },
        ];
        let m = peers_to_map(&rows).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m["laptop"], "laptop.lan:15100");
        assert_eq!(peers_from_map(&m)[0].name, "laptop");

        let bad = vec![
            PeerRow { name: "pc".into(), address: "".into() },
            PeerRow { name: "".into(), address: "1.2.3.4".into() },
            PeerRow { name: "x".into(), address: "1.2.3.4".into() },
            PeerRow { name: "X".into(), address: "1.2.3.5".into() },
        ];
        assert_eq!(peers_to_map(&bad).unwrap_err().len(), 3);
    }

    #[test]
    fn key_and_name_validation() {
        assert!(validate_key("short").is_some());
        assert!(validate_key("abcd efgh ijkl mnop").is_none()); // whitespace is stripped
        assert!(validate_key("abcdefghijklmno").is_some());
        assert!(validate_machine_name("").is_none());
        assert!(validate_machine_name("DESKTOP-ABC").is_none());
        assert!(validate_machine_name("mac.local").is_some());
        assert!(validate_machine_name("Mäc").is_some());
    }

    #[test]
    fn awake_roundtrip() {
        let n = now();
        for c in [
            awake::AwakeConfig::passive(),
            awake::AwakeConfig::indefinite(true),
            awake::AwakeConfig::timed(Duration::from_secs(90 * 60), false),
            awake::AwakeConfig::expirable(Local.with_ymd_and_hms(2026, 10, 25, 18, 30, 0).unwrap(), true),
        ] {
            let e = AwakeEdit::from_config(&c, n);
            assert_eq!(e.to_config(n).unwrap(), c);
            assert!(e.warning(n).is_none());
        }
        let mut e = AwakeEdit::from_config(&awake::AwakeConfig::timed(Duration::from_secs(90 * 60), false), n);
        assert_eq!((e.hours, e.minutes), (1, 30));
        e.hours = 0;
        e.minutes = 0;
        assert!(e.to_config(n).is_err());
        e.kind = AwakeKind::Until;
        e.until = "17:45".into();
        let c = e.to_config(n).unwrap();
        assert_eq!(c.mode, awake::AwakeMode::Expirable(Local.with_ymd_and_hms(2026, 10, 25, 17, 45, 0).unwrap()));
        e.until = "2026-10-24 10:00".into();
        assert!(e.to_config(n).is_ok() && e.warning(n).is_some());
        e.until = "soon".into();
        assert!(e.to_config(n).is_err());
    }

    #[test]
    fn hotkeys() {
        let h = mouse_utils::Hotkey::new(true, false, false, true, 0x48);
        let s = format_hotkey(&h);
        assert_eq!(parse_hotkey(&s).unwrap(), h);
        assert_eq!(parse_hotkey("ctrl+alt+F1").unwrap(), mouse_utils::Hotkey::new(false, true, true, false, 0x70));
        assert_eq!(parse_hotkey(" ").unwrap(), mouse_utils::Hotkey::none());
        assert_eq!(format_hotkey(&mouse_utils::Hotkey::none()), "");
        assert!(parse_hotkey("Ctrl+Shift").is_err());
        assert!(parse_hotkey("Ctrl+A+B").is_err());
        assert!(parse_hotkey("Ctrl+Nope").is_err());
        let c = Combo { ctrl: true, shift: true, vk: 0x56, ..Default::default() };
        assert_eq!(parse_shortcut(&c.to_text()).unwrap(), parse_shortcut("Ctrl+Shift+V").unwrap());
        assert_eq!(Combo { vk: 0x1B, ..Default::default() }.to_text(), "Esc");
    }

    #[test]
    fn remap_rows() {
        let km = keyboard_manager::Config::from_json(
            r#"{"remapKeys":[{"from":"CapsLock","to":"Esc"},{"from":"F13","to":{"text":"hi there"}}],
                "remapShortcuts":[{"from":"Ctrl+Left","to":"Option+Left","app":"com.apple.Terminal"},
                  {"from":"Ctrl+Alt+T","to":{"run":{"path":"/bin/echo","args":["a","b"],"workingDir":"/tmp"}}},
                  {"from":"Ctrl+Shift+P","to":"pastePlainText"},{"from":"Ctrl+Q","to":"disabled"}]}"#,
        )
        .unwrap();
        for r in &km.remap_keys {
            assert_eq!(&RemapRow::from_key_remap(r).key_remap().unwrap(), r);
        }
        for r in &km.remap_shortcuts {
            assert_eq!(&RemapRow::from_shortcut_remap(r).shortcut_remap().unwrap(), r);
        }
        let row = RemapRow::from_shortcut_remap(&km.remap_shortcuts[1]);
        assert_eq!((row.kind, row.args.as_str()), (TargetKind::Run, "a b"));

        let mut r = RemapRow { from: "Nope".into(), value: "Esc".into(), ..RemapRow::new() };
        assert!(r.key_remap().unwrap_err().contains("Nope"));
        assert!(r.shortcut_remap().is_err());
        r.from = "A".into();
        assert!(r.key_remap().is_ok());
        assert!(r.shortcut_remap().is_err()); // a shortcut needs a modifier
        r.value = "disabled".into(); // typed instead of picking "Disabled"
        assert!(r.key_remap().is_err());
        r.kind = TargetKind::Disabled;
        assert_eq!(r.key_remap().unwrap().to, Target::Disabled);
        r.kind = TargetKind::OpenUri;
        r.value = " ".into();
        assert!(r.key_remap().is_err());
        assert!(RemapRow::new().is_blank());
    }

    #[test]
    fn draft_build_and_errors() {
        let n = now();
        let cfg = AppConfig::with_new_key();
        let d = AppDraft::new(cfg.clone(), n);
        let built = d.build(n).unwrap();
        assert_eq!(serde_json::to_value(&built).unwrap(), serde_json::to_value(&cfg).unwrap());

        let mut d = AppDraft::new(cfg, n);
        d.cfg.mwb.settings.key = "short".into();
        d.matrix = slots(["a", "A", "", ""]);
        d.find_hotkey = "Ctrl".into();
        d.key_remaps.push(RemapRow::new()); // blank rows are skipped
        d.key_remaps.push(RemapRow { from: "Bogus".into(), value: "A".into(), ..RemapRow::new() });
        d.paste_hotkey = "V".into();
        let errs = d.build(n).unwrap_err();
        let pages: Vec<Page> = errs.iter().map(|e| e.page).collect();
        assert_eq!(pages, [Page::Mwb, Page::Mwb, Page::MouseUtils, Page::KeyboardManager, Page::KeyboardManager]);

        // A bad key doesn't block saving while MWB is off.
        let mut d = AppDraft::new(AppConfig::default(), n);
        d.cfg.mwb.enabled = false;
        d.cfg.mwb.settings.key.clear();
        let c = d.build(n).unwrap();
        assert!(c.mwb.settings.matrix.is_empty());
        d.matrix = slots(["mac", "", "pc", ""]);
        d.peers.push(PeerRow { name: "pc".into(), address: "10.1.1.1".into() });
        d.cfg.keyboard_manager.paste_plain_text.restore_clipboard = true;
        let c = d.build(n).unwrap();
        assert_eq!(c.mwb.settings.matrix, ["mac", "", "pc"]);
        assert_eq!(c.mwb.settings.peers["pc"], "10.1.1.1");
        assert!(c.keyboard_manager.paste_plain_text.restore_clipboard);
    }

    #[test]
    fn new_sections_round_trip_through_the_draft() {
        let n = now();
        let mut cfg = AppConfig::with_new_key();
        cfg.keyboard_manager.windows_style.enabled = true;
        cfg.keyboard_manager.windows_style.finder_cut_paste = false;
        cfg.keyboard_manager.windows_style.excluded_apps = vec!["com.example.Term".into()];
        cfg.color_picker.settings.hotkey = color_picker::Hotkey::new(true, false, false, true, 0x4B);
        cfg.window_switcher.app_windows_trigger = window_switcher::Hotkey::new(false, true, false, false, 0xC0);
        cfg.display_control.hotkeys.mute = display_control::Hotkey::new(false, true, true, true, 0x4D);
        cfg.mouse_tuning.buttons.mappings.push(mouse_tuning::ButtonMapping {
            button: 2,
            action: mouse_tuning::ButtonAction::Keystroke { keys: "Cmd+W".into() },
        });
        let built = AppDraft::new(cfg.clone(), n).build(n).unwrap();
        assert_eq!(serde_json::to_value(&built).unwrap(), serde_json::to_value(&cfg).unwrap());

        // Bad values are caught on their pages.
        let mut d = AppDraft::new(cfg, n);
        d.switch_trigger = "Tab".into();
        d.color_hotkey = "Ctrl+Nope".into();
        d.cfg.clipboard_history.hotkey = "Cmd+Bogus".into();
        d.cfg.color_picker.settings.copied_format = "Missing".into();
        d.cfg.mouse_tuning.buttons.mappings[0].action = mouse_tuning::ButtonAction::Keystroke { keys: "Cmd+Nope".into() };
        let pages: Vec<Page> = d.build(n).unwrap_err().iter().map(|e| e.page).collect();
        for p in [Page::WindowSwitcher, Page::ColorPicker, Page::ClipboardHistory, Page::MouseTuning] {
            assert!(pages.contains(&p), "{p:?} in {pages:?}");
        }
    }

    #[test]
    fn drag_shelf_round_trips_through_the_draft() {
        let n = now();
        let mut cfg = AppConfig::with_new_key();
        cfg.drag_shelf.sensitivity = drag_shelf::Sensitivity::Custom;
        cfg.drag_shelf.custom_shake.reversals = 99;
        cfg.drag_shelf.max_shelves = 0;
        cfg.drag_shelf.remove_after_drag_out = false;
        let mut d = AppDraft::new(cfg.clone(), n);
        assert_eq!(d.drag_shelf_hotkey, "");
        assert_eq!(Page::from_name("drag-shelf"), Some(Page::DragShelf));
        assert_eq!(Page::DragShelf.section(), Some("Windows & Desktop"));
        assert!(Page::ALL.contains(&Page::DragShelf));
        // Out-of-range values are fixed up on save.
        let c = d.build(n).unwrap();
        assert_eq!((c.drag_shelf.max_shelves, c.drag_shelf.custom_shake.reversals), (1, 10));
        assert!(!c.drag_shelf.remove_after_drag_out);
        // The shortcut is parsed, kept and checked against the others.
        d.drag_shelf_hotkey = "Ctrl+Alt+D".into();
        let c = d.build(n).unwrap();
        assert_eq!(c.drag_shelf.hotkey, drag_shelf::Hotkey::new(false, true, true, false, 0x44));
        let again = AppDraft::new(c.clone(), n);
        assert_eq!(parse_hk(&again.drag_shelf_hotkey), parse_hk(&d.drag_shelf_hotkey));
        if drag_shelf::SUPPORTED {
            assert!(hotkey_uses(&c).iter().any(|u| u.page == Page::DragShelf));
            d.color_hotkey = "Ctrl+Alt+D".into();
            let errs = d.build(n).unwrap_err();
            assert!(errs.iter().any(|e| e.message.contains("New drag shelf") || e.page == Page::DragShelf), "{errs:?}");
            d.color_hotkey = AppDraft::new(cfg.clone(), n).color_hotkey;
        }
        d.drag_shelf_hotkey = "Ctrl+Nope".into();
        let errs = d.build(n).unwrap_err();
        assert_eq!(errs[0].page, Page::DragShelf);
        // Off: no shortcut registered.
        let mut off = c.clone();
        off.drag_shelf.enabled = false;
        assert!(!hotkey_uses(&off).iter().any(|u| u.page == Page::DragShelf));
    }

    #[test]
    fn global_hotkey_conflicts() {
        let n = now();
        // The defaults don't clash, FancyZones' included.
        let mut cfg = AppConfig::with_new_key();
        cfg.clipboard_history.enabled = true;
        let mut d = AppDraft::new(cfg.clone(), n);
        let mut fz = fancyzones::Config::default();
        fz.settings.window_switching = true;
        fz.settings.monitor_rotation = true;
        d.external_hotkeys = fz_hotkey_uses(&fz, true);
        let uses = [d.external_hotkeys.clone(), hotkey_uses(&cfg)].concat();
        assert!(hotkey_conflicts(&uses).is_empty(), "{:?}", hotkey_conflicts(&uses));
        assert!(d.build(n).is_ok());

        // The Color Picker on Cmd+Shift+V clashes with the clipboard history.
        d.color_hotkey = "Cmd+Shift+V".into();
        let errs = d.build(n).unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].page, Page::ColorPicker);
        assert!(errs[0].message.contains("Clipboard history"), "{}", errs[0].message);
        // ... but not while the clipboard history is off.
        d.cfg.clipboard_history.enabled = false;
        assert!(d.build(n).is_ok());

        // A FancyZones shortcut taken by a display hotkey is reported on Displays.
        let editor = fz.settings.editor_hotkey;
        d.display_hotkeys[0] = format_fz_hotkey(&editor);
        let errs = d.build(n).unwrap_err();
        assert_eq!(errs[0].page, Page::Displays);
        assert!(errs[0].message.contains("Layout editor"), "{}", errs[0].message);
        // Not when FancyZones is off.
        d.external_hotkeys = fz_hotkey_uses(&fz, false);
        assert!(d.build(n).is_ok());

        // Same page: the second one is reported.
        let a = HotkeyUse { page: Page::Displays, name: "A".into(), combo: Combo { ctrl: true, vk: 0x41, ..Default::default() } };
        let b = HotkeyUse { name: "B".into(), ..a.clone() };
        let p = hotkey_conflicts(&[a, b]);
        assert_eq!(p.len(), 1);
        assert!(p[0].message.starts_with("B:"));
    }

    #[test]
    fn color_formats_validation() {
        let mut c = color_picker::Config::default();
        assert!(validate_color_formats(&c).is_empty());
        c.custom_formats.push(color_picker::CustomFormat { name: "x".into(), format: "%Re".into() });
        c.custom_formats.push(color_picker::CustomFormat { name: "x".into(), format: "".into() });
        c.copied_format = "x".into();
        assert_eq!(validate_color_formats(&c).len(), 2);
    }

    #[test]
    fn file_tools_save_keeps_rename_history() {
        let dir = std::env::temp_dir().join(format!("pm-settings-ft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file-tools.json");
        let mut c = file_tools::Config::default();
        c.rename.search_mru = vec!["IMG_".into()];
        c.save_to(&path).unwrap();
        let mut e = FtEdit::of(&file_tools::Config::default());
        e.new_plus.open_after_create = true;
        e.mru_size = 3;
        save_file_tools(&path, &e).unwrap();
        let back = file_tools::Config::load_from(&path);
        assert_eq!(back.rename.search_mru, ["IMG_"]);
        assert!(back.new_plus.open_after_create);
        assert_eq!(back.rename.mru_size, 3);
        // Finder basics and Image Resizer settings go through too.
        e.finder.terminal_app = "com.googlecode.iterm2".into();
        e.finder.path_style = file_tools::CopyStyle::Tilde;
        e.resize.presets.push(file_tools::resize::ResizeSize::new(
            "Thumb",
            file_tools::resize::ResizeFit::Fill,
            200.0,
            200.0,
            file_tools::resize::ResizeUnit::Pixel,
        ));
        e.resize.options.jpeg_quality = 70;
        assert!(validate_resize(&e.resize).is_empty());
        save_file_tools(&path, &e).unwrap();
        let back = file_tools::Config::load_from(&path);
        assert_eq!(back.finder.terminal_app, "com.googlecode.iterm2");
        assert_eq!(back.finder.path_style, file_tools::CopyStyle::Tilde);
        assert_eq!(back.resize.presets.last().unwrap().name, "Thumb");
        assert_eq!(back.resize.options.jpeg_quality, 70);
        assert_eq!(back.rename.search_mru, ["IMG_"]);
        assert_eq!(FtEdit::of(&back), e);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resize_presets_validation() {
        use file_tools::resize::{ResizeFit, ResizeSize, ResizeUnit};
        let mut r = file_tools::ResizeConfig::default();
        assert!(validate_resize(&r).is_empty());
        r.presets.push(ResizeSize::new("small", ResizeFit::Fit, 0.0, 0.0, ResizeUnit::Pixel));
        r.presets.push(ResizeSize::new(" ", ResizeFit::Fit, 10.0, 0.0, ResizeUnit::Pixel));
        r.options.jpeg_quality = 0;
        let e = validate_resize(&r);
        assert_eq!(e.len(), 4, "{e:?}");
        assert!(e.iter().any(|m| m.contains("two sizes named")));
    }

    /// Settings the window has no special buffer for (the Caps Lock key,
    /// the new modules' sections) survive a load / build round trip.
    #[test]
    fn new_module_sections_round_trip_through_the_draft() {
        let mut c = AppConfig::with_new_key();
        c.keyboard_manager.hyper_key = keyboard_manager::HyperKeyConfig {
            mode: keyboard_manager::HyperMode::Control,
            tap_action: keyboard_manager::TapAction::CapsLock,
            tap_timeout_ms: 250,
        };
        c.display_scaling.enabled = true;
        c.display_scaling.displays.insert(
            "LG#1".into(),
            display_scaling::DisplayConfig {
                hidpi_enabled: true,
                target_logical_resolution: Some(display_scaling::Resolution { width: 1920, height: 1080 }),
                ..Default::default()
            },
        );
        c.audio.rules.outputs.push(audio_control::DeviceMatch::name("Studio Display"));
        c.audio.block_auto_switch_to.push(audio_control::DeviceMatch::name("AirPods"));
        c.dock_previews.preview_size = dock_previews::PreviewSize::Large;
        c.text_expander.enabled = true;
        c.text_expander.snippets.push(text_expander::Snippet::new(";addr", "1 Infinite Loop"));
        let d = AppDraft::new(c.clone(), now());
        let back = d.build(now()).unwrap();
        assert_eq!(back.keyboard_manager.hyper_key, c.keyboard_manager.hyper_key);
        assert_eq!(back.display_scaling, c.display_scaling);
        assert_eq!(back.audio, c.audio);
        assert_eq!(back.dock_previews, c.dock_previews);
        assert_eq!(back.text_expander, c.text_expander);
    }

    /// Automation hotkeys, routes, the mixer, the battery limit and
    /// gestures survive the draft; hotkeys join the global shortcut check.
    #[test]
    fn automation_sections_round_trip_and_hotkeys_conflict() {
        use keyboard_manager::automation::Action;
        let mut c = AppConfig::with_new_key();
        c.keyboard_manager.enabled = true;
        c.keyboard_manager.hotkeys.push(keyboard_manager::Hotkey {
            from: "Ctrl+Alt+B".parse().unwrap(),
            action: Action::Shell(keyboard_manager::ShellAction { command: "make".into(), ..Default::default() }),
            app: None,
            enabled: true,
        });
        c.keyboard_manager.hotkeys.push(keyboard_manager::Hotkey {
            from: "F13".parse().unwrap(),
            action: Action::OpenUri { uri: "https://example.com".into() },
            app: None,
            enabled: false,
        });
        c.audio.app_routes.insert("com.spotify.client".into(), audio_control::AppRoute::to(audio_control::DeviceMatch::name("AirPods")));
        c.audio.mixer.enabled = true;
        c.battery_limit.limit_percent = 75;
        c.gestures.bindings.push(gestures::GestureBinding::new("3-finger-tap".parse().unwrap(), Action::Shortcut { keys: "Cmd+W".into() }));
        let mut d = AppDraft::new(c.clone(), now());
        assert_eq!(d.hotkeys.len(), 2);
        assert_eq!(d.hotkeys[0].from, c.keyboard_manager.hotkeys[0].from.to_string());
        let back = d.build(now()).unwrap();
        assert_eq!(back.keyboard_manager.hotkeys, c.keyboard_manager.hotkeys);
        assert_eq!(back.audio, c.audio);
        assert_eq!(back.battery_limit, c.battery_limit);
        assert_eq!(back.gestures, c.gestures);
        // In the registry (enabled, global ones only).
        let uses = hotkey_uses(&back);
        assert_eq!(uses.iter().filter(|u| u.page == Page::Automation).count(), 1);
        // The same shortcut as Paste as plain text is a conflict.
        d.hotkeys[0].from = d.paste_hotkey.clone();
        let errs = d.build(now()).unwrap_err();
        assert!(errs.iter().any(|e| e.page == Page::Automation && e.message.contains("already used")), "{errs:?}");
        // A blank row isn't saved; a bad one blocks saving.
        d.hotkeys[0].from = String::new();
        assert_eq!(d.build(now()).unwrap().keyboard_manager.hotkeys.len(), 1);
        d.hotkeys[0].from = "Ctrl+Nope".into();
        assert!(d.build(now()).unwrap_err().iter().any(|e| e.page == Page::Automation));
        // Single keys can't be limited to an app.
        d.hotkeys[0].from = "F14".into();
        d.hotkeys[0].app = "com.apple.Terminal".into();
        assert!(d.build(now()).is_err());
    }

    #[test]
    fn fancyzones_hotkeys_parse_and_validate() {
        use fancyzones::{Hotkey, Settings};
        let d = Settings::default();
        for h in [d.editor_hotkey, d.next_tab_hotkey, d.prev_tab_hotkey, d.monitor_rotation_hotkey] {
            let t = format_fz_hotkey(&h);
            assert_eq!(parse_fz_hotkey(&t).unwrap(), h, "{t}");
            // fancyzones' own spelling is accepted too.
            assert_eq!(parse_fz_hotkey(&h.to_string()).unwrap(), h, "{h}");
        }
        assert_eq!(parse_fz_hotkey("").unwrap(), Hotkey::default());
        assert_eq!(parse_fz_hotkey("none").unwrap().code, 0);
        assert!(parse_fz_hotkey("Ctrl+Nope").is_err());
        assert!(parse_fz_hotkey("Ctrl+A+B").is_err());
        let h = parse_fz_hotkey("Ctrl+Shift+K").unwrap();
        assert!(h.modifiers.ctrl && h.modifiers.shift && !h.modifiers.alt && h.code == 0x4B);

        let mut c = fancyzones::Config::default();
        assert!(validate_fancyzones(&c).is_empty(), "{:?}", validate_fancyzones(&c));
        // Conflicts only count for features that are on.
        c.settings.monitor_rotation_hotkey = c.settings.editor_hotkey;
        assert!(validate_fancyzones(&c).is_empty());
        c.settings.monitor_rotation = true;
        assert_eq!(validate_fancyzones(&c).len(), 1);
        c.settings.monitor_rotation_hotkey = Hotkey::default();
        assert!(validate_fancyzones(&c)[0].message.contains("pick a shortcut"));
        c.settings.monitor_rotation = false;
        c.settings.next_tab_hotkey = Hotkey::new(fancyzones::Modifiers::default(), 0x41);
        assert!(validate_fancyzones(&c)[0].message.contains("modifier"));
        c.settings.window_switching = false;
        c.settings.layout_name_label_text_color = "white".into();
        c.settings.excluded_apps = vec!["  ".into()];
        assert_eq!(validate_fancyzones(&c).len(), 2);
    }

    #[test]
    fn fancyzones_save_keeps_history() {
        use fancyzones::{AppliedLayout, LayoutType};
        let dir = std::env::temp_dir().join(format!("pm-settings-fz-{}", std::process::id()));
        let path = fancyzones_path(&dir.join("config.json"));
        assert!(path.ends_with("fancyzones.json"));
        let mut disk = fancyzones::Config::default();
        disk.app_zone_history.insert(
            "/Applications/Safari.app".into(),
            fancyzones::config::AppZoneHistoryEntry {
                monitor: "m".into(),
                layout_uuid: "u".into(),
                layout_kind: LayoutType::Grid,
                zones: vec![1],
            },
        );
        disk.save_to(&path).unwrap();

        let mut edit = FzEdit::of(&fancyzones::Config::default());
        edit.settings.shift_drag = false;
        edit.applied_layouts.insert("m".into(), AppliedLayout::template(LayoutType::Columns, 2));
        save_fancyzones(&path, &edit).unwrap();
        let back = load_fancyzones(&path).unwrap();
        assert!(!back.settings.shift_drag);
        assert_eq!(back.applied_layouts["m"].kind, LayoutType::Columns);
        assert_eq!(back.app_zone_history.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn layout_validation() {
        use fancyzones::{AppliedLayout, LayoutType};
        assert!(validate_layout(&AppliedLayout::template(LayoutType::Grid, 4), &[]).is_none());
        assert!(validate_layout(&AppliedLayout::template(LayoutType::Columns, 0), &[]).is_some());
        assert!(validate_layout(&AppliedLayout::template(LayoutType::Blank, 0), &[]).is_none());
        let mut c = fancyzones::Config::default();
        assert!(validate_fancyzones(&c).is_empty());
        c.applied_layouts.insert("m".into(), AppliedLayout::template(LayoutType::Rows, 0));
        assert_eq!(validate_fancyzones(&c).len(), 1);
    }
}
