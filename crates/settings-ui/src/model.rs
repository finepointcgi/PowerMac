//! The editable state behind the settings window, and everything about it
//! that isn't drawing: text buffers for values that need parsing, validation,
//! the MWB matrix operations, and turning the draft back into config files.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
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
    ScrollReverser,
    MouseUtils,
    KeyboardManager,
}

impl Page {
    pub const ALL: [Page; 7] = [
        Page::General,
        Page::Mwb,
        Page::FancyZones,
        Page::Awake,
        Page::ScrollReverser,
        Page::MouseUtils,
        Page::KeyboardManager,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Mwb => "Mouse Without Borders",
            Page::FancyZones => "FancyZones",
            Page::Awake => "Awake",
            Page::ScrollReverser => "Scroll Reverser",
            Page::MouseUtils => "Mouse Utilities",
            Page::KeyboardManager => "Keyboard Manager",
        }
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

        if errs.is_empty() {
            Ok(c)
        } else {
            Err(errs)
        }
    }
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
pub fn save_fancyzones(path: &Path, edit: &FzEdit) -> Result<()> {
    let mut c = load_fancyzones(path).unwrap_or_default();
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

pub fn validate_fancyzones(c: &fancyzones::Config) -> Vec<Problem> {
    let mut out = Vec::new();
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
