//! # fancyzones — a Rust port of PowerToys FancyZones for macOS and Windows
//!
//! * [`layout`] / [`navigation`] / [`engine`] / [`config`] are platform
//!   independent (zone math identical to FancyZones' `LayoutConfigurator`,
//!   hit-testing from `Layout::ZonesFromPoint`, keyboard navigation from
//!   `WindowKeyboardSnap`, JSON compatible with FancyZones' files).
//! * [`platform`] contains the OS backends (Accessibility + CGEventTap +
//!   AppKit overlay on macOS; WinEvent hooks + low-level keyboard hook +
//!   layered GDI overlay on Windows).
//!
//! ## Integrating into an app binary
//!
//! ```no_run
//! let config = fancyzones::Config::load_or_default().unwrap();
//! let handle = fancyzones::FancyZones::start(config).unwrap();
//! // ... on macOS the MAIN thread must now run the AppKit run loop:
//! #[cfg(target_os = "macos")]
//! fancyzones::platform::run_main_loop(); // or your own NSApplication::run()
//! drop(handle); // stops hooks/threads
//! ```
//!
//! **macOS threading:** `FancyZones::start` may be called from any thread. It
//! spawns an event-tap thread (CFRunLoop) and a worker thread (Accessibility
//! calls). The zone overlay is AppKit UI and is created/updated by blocks
//! posted to the *main dispatch queue*; therefore the process' main thread
//! must run an `NSApplication` run loop (`NSApplication::run`, or
//! [`platform::run_main_loop`]) or at least a `CFRunLoop` that services the
//! main queue. Without it, keyboard snapping and drop-to-zone still work but
//! no overlay is drawn. The process needs the *Accessibility* permission
//! (System Settings → Privacy & Security → Accessibility) for both the event
//! tap and moving windows; `start` returns an error if it is missing.
//!
//! **Windows threading:** `start` spawns one thread that owns the WinEvent
//! hook, the `WH_KEYBOARD_LL` hook and the overlay windows and pumps their
//! message loop. Nothing is required from the caller's thread.
//!
//! ## Hotkeys
//!
//! Hotkeys are stored as [`Hotkey`] (modifiers + Windows VK code). The macOS
//! defaults replace Win with Ctrl+Option where Cmd would collide with system
//! shortcuts:
//!
//! | Action                      | Windows (PowerToys)     | macOS default            |
//! |-----------------------------|-------------------------|--------------------------|
//! | Snap to zone                | Win+Arrow               | Ctrl+Option+Arrow        |
//! | Extend zones                | Win+Ctrl+Alt+Arrow      | Ctrl+Option+Cmd+Arrow    |
//! | Open layout editor          | Win+Shift+`             | Ctrl+Option+`            |
//! | Next window in zone         | Win+PgDn                | Ctrl+Option+]            |
//! | Previous window in zone     | Win+PgUp                | Ctrl+Option+[            |
//! | Quick layout switch (fixed) | Win+Ctrl+Alt+0..9       | Ctrl+Option+Cmd+0..9     |
//! | Monitor rotation (hold)     | Alt+X, then Left/Right  | Ctrl+Option+X, then Left/Right |
//!
//! (Cmd+Shift+` is macOS' "previous window of app", Cmd+PgUp/PgDn is page
//! scrolling and laptops have no PgUp/PgDn keys, Option+X types "≈".)
//!
//! ## Platform notes
//!
//! * `make_dragged_window_transparent` works on Windows only: macOS offers no
//!   public way to change the alpha of another app's window (the private
//!   `CGSSetWindowAlpha` only works for the owning connection or with code
//!   injected into the Dock), so the setting is a no-op there.
//! * `disable_round_corners` uses the Windows 11 DWM corner preference; no-op
//!   elsewhere.
//! * macOS display / work-area / Space changes are detected by polling
//!   `NSScreen` and the private `CGSManagedDisplayGetCurrentSpace` once a
//!   second; Windows uses `WM_DISPLAYCHANGE` / `WM_SETTINGCHANGE` and the
//!   virtual desktop id in the registry.
//! * New windows: macOS attaches an `AXObserver` (`kAXWindowCreatedNotification`)
//!   to every regular app (re-scanned every second); Windows uses
//!   `EVENT_OBJECT_SHOW`.
//! * Zone assignments live in memory only (PowerToys stores them as window
//!   properties), so they do not survive a restart.

pub mod config;
pub mod edge_snap;
pub mod engine;
pub mod filters;
pub mod geometry;
pub mod keys;
pub mod layout;
pub mod navigation;
pub mod platform;
pub mod rotation;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use anyhow::Result;

pub use config::{Config, Hotkey, LayoutHotkey, LayoutNameLabelPlacement, Modifiers, Settings};
pub use filters::{AppIdentity, WindowKind};
pub use edge_snap::{BottomEdgeAction, EdgeAction, TopEdgeAction};
pub use engine::{DragResult, Engine, LabelState, Monitor, SwitchFeedback, WindowSystem, WorkArea};
pub use geometry::{Point, Rect};
pub use layout::{AppliedLayout, CustomLayout, CustomLayoutInfo, Layout, LayoutType};
pub use navigation::Direction;

/// Called with the id of the monitor under the cursor when the editor
/// hotkey is pressed (from a FancyZones thread; keep it short).
pub type EditorCallback = Box<dyn Fn(Option<String>) + Send + Sync>;

/// Options for [`FancyZones::start_with`].
pub struct StartOptions {
    /// Where configuration changes are saved (`None` = don't persist).
    pub config_path: Option<PathBuf>,
    /// Invoked when `settings.editor_hotkey` is pressed. The crate has no
    /// editor of its own; the app is expected to launch one.
    pub on_open_editor: Option<EditorCallback>,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self { config_path: Config::default_path(), on_open_editor: None }
    }
}

impl std::fmt::Debug for StartOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartOptions")
            .field("config_path", &self.config_path)
            .field("on_open_editor", &self.on_open_editor.as_ref().map(|_| "Fn"))
            .finish()
    }
}

/// Non-arrow hotkeys, readable from the hook callbacks.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct KeyBindings {
    pub editor: Option<Hotkey>,
    pub next_tab: Option<Hotkey>,
    pub prev_tab: Option<Hotkey>,
    pub rotation: Option<Hotkey>,
    /// Bit `k` set: quick-switch key `k` is bound to an existing custom layout.
    pub layout_keys: u16,
    /// Mouse wheel layout switching is on and has at least two layouts to cycle.
    pub wheel: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyAction {
    Editor,
    NextTab,
    PrevTab,
    QuickLayout(u8),
}

/// Modifiers of the (fixed) quick layout switch chord: Win+Ctrl+Alt /
/// Ctrl+Option+Cmd.
pub const QUICK_SWITCH_MODIFIERS: Modifiers = Modifiers::new(true, true, false, true);

impl KeyBindings {
    pub fn from_config(c: &Config) -> Self {
        let s = &c.settings;
        let e = Engine::new(Config { settings: s.clone(), custom_layouts: c.custom_layouts.clone(), layout_hotkeys: c.layout_hotkeys.clone(), ..Default::default() });
        let mut layout_keys = 0u16;
        if s.quick_layout_switch {
            for k in 0..10u8 {
                if e.quick_layout(k).is_some() {
                    layout_keys |= 1 << k;
                }
            }
        }
        let some = |h: Hotkey| (h.code != 0).then_some(h);
        KeyBindings {
            editor: some(s.editor_hotkey),
            next_tab: if s.window_switching { some(s.next_tab_hotkey) } else { None },
            prev_tab: if s.window_switching { some(s.prev_tab_hotkey) } else { None },
            rotation: if s.monitor_rotation { some(s.monitor_rotation_hotkey) } else { None },
            layout_keys,
            wheel: s.mouse_wheel_layout_switch && e.wheel_cycle_ids().len() >= 2,
        }
    }

    /// Classify a key press. `is_key(vk)` tells whether the pressed key is
    /// the given Windows virtual key; `digit` is the digit of the pressed key.
    pub fn classify(&self, m: Modifiers, is_key: impl Fn(u16) -> bool, digit: Option<u8>) -> Option<KeyAction> {
        let hit = |h: &Option<Hotkey>| h.is_some_and(|h| h.code != 0 && h.modifiers == m && is_key(h.code));
        if let Some(d) = digit {
            if m == QUICK_SWITCH_MODIFIERS && self.layout_keys & (1 << d) != 0 {
                return Some(KeyAction::QuickLayout(d));
            }
        }
        if hit(&self.editor) {
            Some(KeyAction::Editor)
        } else if hit(&self.next_tab) {
            Some(KeyAction::NextTab)
        } else if hit(&self.prev_tab) {
            Some(KeyAction::PrevTab)
        } else {
            None
        }
    }

    /// `m` + key exactly is the monitor rotation chord.
    pub fn is_rotation_chord(&self, m: Modifiers, is_key: impl Fn(u16) -> bool) -> bool {
        self.rotation.is_some_and(|h| h.modifiers == m && is_key(h.code))
    }
}

/// Work posted to the platform thread.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Command {
    /// A monitor's layout changed through the API (tray / settings reload).
    LayoutChanged(String),
    /// Quick layout key pressed / requested.
    QuickLayout(u8),
    /// Rotate windows across monitors.
    Rotate(bool),
}

/// Packed hotkey settings readable from hook callbacks without locking.
pub(crate) mod hotkey_bits {
    use crate::config::{Modifiers, Settings};
    pub const ENABLED: u32 = 1;
    pub const POSITION: u32 = 2;
    fn mods(m: &Modifiers) -> u32 {
        (m.ctrl as u32) | (m.alt as u32) << 1 | (m.shift as u32) << 2 | (m.meta as u32) << 3
    }
    pub fn pack(s: &Settings) -> u32 {
        let mut v = 0;
        if s.override_snap_hotkeys {
            v |= ENABLED;
        }
        if s.move_windows_based_on_position {
            v |= POSITION;
        }
        v | mods(&s.snap_modifiers) << 4 | mods(&s.extend_modifiers) << 8
    }
    /// What a key press with modifiers `m` means, given packed bits.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Action {
        Snap,
        Extend,
    }
    pub fn classify(bits: u32, m: Modifiers, horizontal: bool) -> Option<Action> {
        if bits & ENABLED == 0 {
            return None;
        }
        let pos = bits & POSITION != 0;
        let mm = mods(&m);
        if mm == (bits >> 4) & 0xF && mm != 0 && (pos || horizontal) {
            return Some(Action::Snap);
        }
        // Extending is position based in FancyZones; only active in that mode.
        if pos && mm == (bits >> 8) & 0xF && mm != 0 {
            return Some(Action::Extend);
        }
        None
    }
}

pub(crate) struct Shared {
    pub engine: Mutex<Engine>,
    pub config_path: Option<PathBuf>,
    pub hotkeys: AtomicU32,
    pub keys: RwLock<KeyBindings>,
    /// A drag currently shows zones (for the wheel hook decision).
    pub zones_shown: AtomicBool,
    pub on_open_editor: Option<EditorCallback>,
    pub commands: Mutex<Vec<Command>>,
    /// Wakes the platform thread to process `commands` (set by the runtime).
    pub waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Shared {
    pub fn new(config: Config, config_path: Option<PathBuf>, on_open_editor: Option<EditorCallback>) -> Self {
        Shared {
            hotkeys: AtomicU32::new(hotkey_bits::pack(&config.settings)),
            keys: RwLock::new(KeyBindings::from_config(&config)),
            engine: Mutex::new(Engine::new(config)),
            config_path,
            zones_shown: AtomicBool::new(false),
            on_open_editor,
            commands: Mutex::new(Vec::new()),
            waker: Mutex::new(None),
        }
    }

    pub fn engine(&self) -> MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn keys(&self) -> KeyBindings {
        self.keys.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Recompute hook-visible state after the config changed.
    pub fn refresh_bindings(&self, cfg: &Config) {
        self.hotkeys.store(hotkey_bits::pack(&cfg.settings), Ordering::SeqCst);
        *self.keys.write().unwrap_or_else(|e| e.into_inner()) = KeyBindings::from_config(cfg);
    }

    pub fn post(&self, cmd: Command) {
        self.commands.lock().unwrap_or_else(|e| e.into_inner()).push(cmd);
        if let Some(w) = self.waker.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            w();
        }
    }

    pub fn take_commands(&self) -> Vec<Command> {
        std::mem::take(&mut *self.commands.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Fire the editor callback.
    pub fn open_editor(&self, monitor_id: Option<String>) {
        match &self.on_open_editor {
            Some(cb) => cb(monitor_id),
            None => log::info!("fancyzones: editor hotkey pressed but no editor is configured"),
        }
    }

    /// Execute a posted command; returns visual feedback for the platform.
    pub fn run_command<WS: WindowSystem>(&self, ws: &WS, cmd: Command, cursor: Option<geometry::Point>) -> SwitchFeedback {
        let mut e = self.engine();
        let fb = match cmd {
            Command::LayoutChanged(id) => match e.on_layout_changed(ws, &id) {
                Ok(_) => {
                    let flash = e.config.settings.zone_set_change_flash_zones && e.drag.is_none();
                    let areas = if flash { e.areas(ws).unwrap_or_default() } else { vec![] };
                    SwitchFeedback { flash: flash.then(|| e.flash_overlay(&areas)), label: None }
                }
                Err(err) => {
                    log::warn!("fancyzones: {err:#}");
                    SwitchFeedback::default()
                }
            },
            Command::QuickLayout(k) => {
                let Some(c) = cursor else { return SwitchFeedback::default() };
                match e.apply_quick_layout(ws, k, c) {
                    Ok(f) => f.unwrap_or_default(),
                    Err(err) => {
                        log::warn!("fancyzones: quick layout switch failed: {err:#}");
                        SwitchFeedback::default()
                    }
                }
            }
            Command::Rotate(reverse) => {
                match e.rotate_windows(ws, reverse) {
                    Ok(n) => log::info!("fancyzones: rotated {n} windows across monitors"),
                    Err(err) => log::warn!("fancyzones: monitor rotation failed: {err:#}"),
                }
                SwitchFeedback::default()
            }
        };
        self.persist(&e.config);
        fb
    }

    pub fn persist(&self, cfg: &Config) {
        if let Some(p) = &self.config_path {
            if let Err(e) = cfg.save_to(p) {
                log::warn!("fancyzones: saving config failed: {e:#}");
            }
        }
    }
}

/// Entry point.
pub struct FancyZones;

impl FancyZones {
    /// Start FancyZones with the config persisted to [`Config::default_path`].
    pub fn start(config: Config) -> Result<Handle> {
        Self::start_with(config, StartOptions::default())
    }

    pub fn start_with(config: Config, opts: StartOptions) -> Result<Handle> {
        let shared = Arc::new(Shared::new(config, opts.config_path, opts.on_open_editor));
        let runtime = platform::Runtime::start(shared.clone())?;
        Ok(Handle { shared, runtime: Some(runtime) })
    }
}

/// Running FancyZones instance. Dropping it stops all hooks and threads.
pub struct Handle {
    shared: Arc<Shared>,
    runtime: Option<platform::Runtime>,
}

impl Handle {
    pub fn stop(&mut self) {
        if let Some(mut rt) = self.runtime.take() {
            rt.stop();
        }
    }

    pub fn is_running(&self) -> bool {
        self.runtime.is_some()
    }

    /// Apply a layout to the monitor with id `monitor_id` (see [`Handle::monitors`])
    /// on the current virtual desktop and persist it. Windows snapped there
    /// are moved (`zone_set_change_move_windows`) and zones flashed
    /// (`zone_set_change_flash_zones`) asynchronously.
    pub fn apply_layout(&self, monitor_id: &str, layout: AppliedLayout) -> Result<()> {
        {
            let mut e = self.shared.engine();
            if layout.kind == LayoutType::Custom && layout::find_custom(&e.config.custom_layouts, &layout.uuid).is_none() {
                anyhow::bail!("custom layout {} not found", layout.uuid);
            }
            e.apply_layout(monitor_id, layout);
            self.shared.persist(&e.config);
        }
        self.shared.post(Command::LayoutChanged(monitor_id.to_string()));
        Ok(())
    }

    /// Apply template `kind` (with the user's `layout_templates` customization).
    pub fn apply_template(&self, monitor_id: &str, kind: LayoutType) -> Result<()> {
        let l = self.shared.engine().config.template(kind);
        self.apply_layout(monitor_id, l)
    }

    /// Add/replace a custom layout and persist it.
    pub fn upsert_custom_layout(&self, layout: CustomLayout) {
        let mut e = self.shared.engine();
        e.config.upsert_custom_layout(layout);
        self.shared.refresh_bindings(&e.config);
        self.shared.persist(&e.config);
    }

    /// Replace the quick layout switch keys and persist them.
    pub fn set_layout_hotkeys(&self, hotkeys: Vec<LayoutHotkey>) {
        let mut e = self.shared.engine();
        e.config.layout_hotkeys = hotkeys;
        self.shared.refresh_bindings(&e.config);
        self.shared.persist(&e.config);
    }

    /// Replace the customized templates and persist them.
    pub fn set_layout_templates(&self, templates: Vec<AppliedLayout>) {
        let mut e = self.shared.engine();
        e.config.layout_templates = templates;
        self.shared.persist(&e.config);
    }

    /// Same as pressing the quick layout switch key `key` (0..=9).
    pub fn apply_quick_layout(&self, key: u8) {
        self.shared.post(Command::QuickLayout(key));
    }

    /// Same as the monitor rotation hotkey: move all windows to the next
    /// (`reverse`: previous) monitor.
    pub fn rotate_monitors(&self, reverse: bool) {
        self.shared.post(Command::Rotate(reverse));
    }

    /// Focus the next / previous window in the focused window's zone(s).
    pub fn cycle_windows(&self, reverse: bool) -> Result<bool> {
        let ws = platform::NativeWindowSystem::new();
        self.shared.engine().cycle_windows(&ws, reverse)
    }

    /// Same as the editor hotkey: fires `StartOptions::on_open_editor` with
    /// the monitor under the cursor.
    pub fn open_editor(&self) {
        self.shared.open_editor(platform::monitor_under_cursor());
    }

    /// Keyboard-snap the focused window (same as the hotkey).
    pub fn snap_focused(&self, direction: Direction) -> Result<bool> {
        let ws = platform::NativeWindowSystem::new();
        let r = self.shared.engine().snap_focused(&ws, direction);
        self.persist_history();
        r
    }

    /// Extend the focused window's zones towards `direction`.
    pub fn extend_focused(&self, direction: Direction) -> Result<bool> {
        let ws = platform::NativeWindowSystem::new();
        self.shared.engine().extend_focused(&ws, direction)
    }

    /// Snap the focused window into zone `zone` (0-based) of `monitor_id`
    /// (or of the monitor it is currently on).
    pub fn snap_focused_to_zone(&self, monitor_id: Option<&str>, zone: usize) -> Result<bool> {
        let ws = platform::NativeWindowSystem::new();
        let r = self.shared.engine().snap_focused_to_zone(&ws, monitor_id, zone);
        self.persist_history();
        r
    }

    pub fn monitors(&self) -> Result<Vec<Monitor>> {
        platform::NativeWindowSystem::new().monitors()
    }

    /// Monitors with their instantiated layouts (global zone rects).
    pub fn work_areas(&self) -> Result<Vec<WorkArea>> {
        let m = self.monitors()?;
        Ok(self.shared.engine().work_areas(&m))
    }

    pub fn config(&self) -> Config {
        self.shared.engine().config.clone()
    }

    /// Change settings (hotkeys etc. take effect immediately) and persist.
    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let mut e = self.shared.engine();
        f(&mut e.config.settings);
        self.shared.refresh_bindings(&e.config);
        self.shared.persist(&e.config);
    }

    fn persist_history(&self) {
        let e = self.shared.engine();
        self.shared.persist(&e.config);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::hotkey_bits::*;
    use super::*;

    #[test]
    fn hotkey_classification() {
        let mut s = Settings {
            snap_modifiers: Modifiers::new(true, true, false, false),
            extend_modifiers: Modifiers::new(true, true, false, true),
            ..Default::default()
        };
        let b = pack(&s);
        let snap = Modifiers::new(true, true, false, false);
        let ext = Modifiers::new(true, true, false, true);
        assert_eq!(classify(b, snap, true), Some(Action::Snap));
        assert_eq!(classify(b, snap, false), None); // up/down need position mode
        assert_eq!(classify(b, ext, true), None);
        assert_eq!(classify(b, Modifiers::new(true, false, false, false), true), None);
        s.move_windows_based_on_position = true;
        let b = pack(&s);
        assert_eq!(classify(b, snap, false), Some(Action::Snap));
        assert_eq!(classify(b, ext, false), Some(Action::Extend));
        s.override_snap_hotkeys = false;
        assert_eq!(classify(pack(&s), snap, true), None);
    }

    fn custom(uuid: &str, name: &str) -> CustomLayout {
        use crate::layout::{CustomLayoutInfo, GridLayoutInfo};
        CustomLayout {
            uuid: uuid.into(),
            name: name.into(),
            info: CustomLayoutInfo::Grid(GridLayoutInfo::new(vec![10000], vec![5000, 5000], vec![vec![0, 1]])),
        }
    }

    #[test]
    fn key_bindings() {
        use crate::keys::vk;
        let mut c = Config::default();
        c.custom_layouts.push(custom("{A}", "a"));
        c.layout_hotkeys.push(LayoutHotkey { key: 3, layout_uuid: "{A}".into() });
        c.layout_hotkeys.push(LayoutHotkey { key: 4, layout_uuid: "{missing}".into() });
        let k = KeyBindings::from_config(&c);
        assert_eq!(k.layout_keys, 1 << 3);
        assert!(!k.wheel);
        let is = |code: u16| move |v: u16| v == code;
        assert_eq!(k.classify(QUICK_SWITCH_MODIFIERS, is(vk::KEY_0 + 3), Some(3)), Some(KeyAction::QuickLayout(3)));
        assert_eq!(k.classify(QUICK_SWITCH_MODIFIERS, is(vk::KEY_0 + 4), Some(4)), None);
        assert_eq!(k.classify(Modifiers::new(true, true, false, false), is(vk::KEY_0 + 3), Some(3)), None);
        let ed = c.settings.editor_hotkey;
        assert_eq!(k.classify(ed.modifiers, is(ed.code), None), Some(KeyAction::Editor));
        let nt = c.settings.next_tab_hotkey;
        assert_eq!(k.classify(nt.modifiers, is(nt.code), None), Some(KeyAction::NextTab));
        let pt = c.settings.prev_tab_hotkey;
        assert_eq!(k.classify(pt.modifiers, is(pt.code), None), Some(KeyAction::PrevTab));
        // rotation is off by default
        let rh = c.settings.monitor_rotation_hotkey;
        assert!(!k.is_rotation_chord(rh.modifiers, is(rh.code)));
        c.settings.monitor_rotation = true;
        c.settings.window_switching = false;
        c.settings.quick_layout_switch = false;
        c.settings.mouse_wheel_layout_switch = true;
        c.custom_layouts.push(custom("{B}", "b"));
        let k = KeyBindings::from_config(&c);
        assert!(k.is_rotation_chord(rh.modifiers, is(rh.code)));
        assert_eq!(k.classify(nt.modifiers, is(nt.code), None), None);
        assert_eq!(k.layout_keys, 0);
        assert!(k.wheel);
    }
}
