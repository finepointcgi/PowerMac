//! The platform-independent remap state machine.
//!
//! The backends feed every physical key event (Windows VK + down/up; key
//! repeat is a repeated down) into [`Engine::process`], which answers either
//! [`Outcome::Pass`] (deliver the event unchanged) or [`Outcome::Replace`]
//! (swallow it and perform these [`Action`]s instead, in order; an empty list
//! just swallows it).
//!
//! Two layers, mirroring PowerToys Keyboard Manager
//! (`KeyboardManagerEngineLibrary/KeyboardEventHandlers.cpp`):
//!
//! 1. **Single key remaps** turn a physical key into a *logical* key (or a
//!    shortcut / text / nothing). The target chosen at key-down is remembered,
//!    so the key-up always releases what the key-down pressed even if the
//!    config changed in between.
//! 2. **Shortcut remaps** watch the logical keyboard. When the action key of a
//!    shortcut goes down with exactly its modifiers held, the remap becomes
//!    *active*:
//!    - shortcut -> shortcut: the source modifiers the target doesn't share are
//!      released (suppressed) at the OS level, the target's are pressed, then
//!      the target action key. Re-pressing the action key while the modifiers
//!      are still held repeats the target (Ctrl held, Left Left Left).
//!    - releasing a source modifier ends it: the target action and modifiers
//!      are released and any source modifiers still physically held are
//!      pressed again, so the OS view matches the physical keyboard (no stuck
//!      modifiers).
//!    - pressing any other key ends it the same way, then that key is
//!      processed afresh (so Ctrl+Left -> Alt+Left, then Shift joins, behaves
//!      as Ctrl+Shift).
//!    - shortcut -> key, -> text, -> paste-plain-text work the same way;
//!      shortcut -> disabled / run program / open URI just swallow the action
//!      key and leave the modifiers alone.
//!
//! The engine tracks three key sets: `physical` (what the user holds),
//! `logical` (after layer 1) and `os` (what the OS has been told is down).
//! Every emitted key-up is for a key in `os`, and once everything is
//! physically released `os` is empty again.
//!
//! On Windows the engine can emit a dummy key (`VK_DUMMY`, 0xFF) around
//! suppressed Win/Alt releases, exactly like PowerToys, so a lone Win or Alt
//! release doesn't open the Start menu / menu bar.

use std::collections::{BTreeSet, HashMap};

use pm_input::vk::*;

use crate::config::{Config, RunProgram, Target};
use crate::keys::{
    concrete, is_modifier, mod_family, ModFamily, Mods, Shortcut, Side, VK_DISABLED, VK_DUMMY,
    VK_WIN_BOTH,
};

/// Something the backend must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Inject a key event (sided Windows VK).
    Key { vk: u32, down: bool },
    /// Type a string.
    Text(String),
    /// Start a program (asynchronously).
    Run(RunProgram),
    /// Open a URL / file (asynchronously).
    OpenUri(String),
    /// Paste the clipboard as plain text (asynchronously).
    PastePlainText,
}

impl Action {
    pub fn down(vk: u32) -> Action {
        Action::Key { vk, down: true }
    }
    pub fn up(vk: u32) -> Action {
        Action::Key { vk, down: false }
    }
}

/// What to do with a physical event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Deliver the original event unchanged.
    Pass,
    /// Swallow the original event and perform these actions instead.
    Replace(Vec<Action>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOptions {
    /// Emit `VK_DUMMY` around suppressed Win/Alt releases (Windows).
    pub dummy_key: bool,
}

#[allow(clippy::derivable_impls)] // not derivable on Windows
impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            dummy_key: cfg!(windows),
        }
    }
}

#[derive(Debug, Clone)]
struct ShortcutEntry {
    from: Shortcut,
    to: Target,
    /// Normalized app name (see [`normalize_app`]).
    app: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Compiled {
    keys: HashMap<u32, Target>,
    shortcuts: Vec<ShortcutEntry>,
}

/// What a held physical key is producing (layer 1).
#[derive(Debug, Clone)]
enum Single {
    /// Produces this logical key.
    Map(u32),
    /// Produces nothing.
    Swallow,
    /// Types this text on every key-down.
    Text(String),
    /// Key -> shortcut; `pressed` are the modifiers we pressed for it.
    Shortcut { target: Shortcut, pressed: Vec<u32> },
}

#[derive(Debug, Clone)]
struct Active {
    from: Shortcut,
    to: Target,
    /// The source action key (logical VK).
    action: u32,
    /// The sided source modifiers held when it fired.
    src_mods: Vec<u32>,
    /// Target key currently held at the OS level (shortcut/key targets).
    target_down: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dummy {
    None,
    BeforeRelease,
    AfterPress,
}

/// The remap state machine. See the module docs.
#[derive(Debug, Clone)]
pub struct Engine {
    opts: EngineOptions,
    compiled: Compiled,
    physical: BTreeSet<u32>,
    single: HashMap<u32, Single>,
    logical: BTreeSet<u32>,
    os: BTreeSet<u32>,
    active: Option<Active>,
}

/// Normalize an app identifier for comparison: lower-case, file name only,
/// without `.exe` / `.app`.
pub fn normalize_app(s: &str) -> String {
    let s = s.trim();
    let s = s.rsplit(['/', '\\']).next().unwrap_or(s);
    let l = s.to_lowercase();
    l.strip_suffix(".exe")
        .or_else(|| l.strip_suffix(".app"))
        .unwrap_or(&l)
        .to_string()
}

fn expand_source(vk: u32) -> Vec<u32> {
    match vk {
        VK_SHIFT => vec![VK_LSHIFT, VK_RSHIFT],
        VK_CONTROL => vec![VK_LCONTROL, VK_RCONTROL],
        VK_MENU => vec![VK_LMENU, VK_RMENU],
        VK_WIN_BOTH => vec![VK_LWIN, VK_RWIN],
        v => vec![v],
    }
}

fn normalize_target(t: &Target) -> Target {
    match t {
        Target::Key(VK_DISABLED) => Target::Disabled,
        Target::Key(k) => Target::Key(concrete(*k)),
        other => other.clone(),
    }
}

fn compile(cfg: &Config) -> Compiled {
    let mut c = Compiled::default();
    if cfg.enabled {
        for r in &cfg.remap_keys {
            let to = normalize_target(&r.to);
            for src in expand_source(r.from.0) {
                if src == 0 || src > 0xFF {
                    log::warn!("keyboard-manager: ignoring remap of invalid key {src:#x}");
                    continue;
                }
                if matches!(to, Target::Key(t) if t == src) {
                    continue;
                }
                c.keys.entry(src).or_insert_with(|| to.clone());
            }
        }
        for r in &cfg.remap_shortcuts {
            c.shortcuts.push(ShortcutEntry {
                from: r.from,
                to: normalize_target(&r.to),
                app: r
                    .app
                    .as_deref()
                    .map(normalize_app)
                    .filter(|a| !a.is_empty()),
            });
        }
    }
    let p = &cfg.paste_plain_text;
    if p.enabled {
        c.shortcuts.push(ShortcutEntry {
            from: p.hotkey,
            to: Target::PastePlainText,
            app: None,
        });
    }
    c
}

/// Physical events normally carry sided VKs; map generic ones to the left key.
fn normalize_vk(vk: u32) -> u32 {
    match vk {
        VK_SHIFT => VK_LSHIFT,
        VK_CONTROL => VK_LCONTROL,
        VK_MENU => VK_LMENU,
        v => v,
    }
}

fn mod_order(vk: u32) -> (usize, u32) {
    let f = mod_family(vk).map_or(9, |f| {
        ModFamily::ALL.iter().position(|&x| x == f).unwrap_or(9)
    });
    (f, vk)
}

fn is_win_or_alt(vk: u32) -> bool {
    matches!(mod_family(vk), Some(ModFamily::Win | ModFamily::Alt))
}

/// Sided modifier keys for a target's modifiers, preferring the side the user
/// is holding for shared "either side" modifiers (PowerToys'
/// `modifierKeysInvoked`).
fn resolve_mods(mods: &Mods, held: &[u32]) -> Vec<u32> {
    let mut v = Vec::new();
    for f in ModFamily::ALL {
        let (l, r) = f.sided();
        match mods.get(f) {
            Side::None => {}
            Side::Left => v.push(l),
            Side::Right => v.push(r),
            Side::Either => v.push(if held.contains(&r) && !held.contains(&l) {
                r
            } else {
                l
            }),
        }
    }
    v
}

impl Engine {
    pub fn new(cfg: &Config, opts: EngineOptions) -> Self {
        Self {
            opts,
            compiled: compile(cfg),
            physical: BTreeSet::new(),
            single: HashMap::new(),
            logical: BTreeSet::new(),
            os: BTreeSet::new(),
            active: None,
        }
    }

    /// Swap in a new config. Keys currently held keep the behaviour they were
    /// pressed with until released.
    pub fn set_config(&mut self, cfg: &Config) {
        self.compiled = compile(cfg);
    }

    /// Keys the user physically holds (as reported to [`process`](Self::process)).
    pub fn physical_keys(&self) -> &BTreeSet<u32> {
        &self.physical
    }

    /// Keys the OS has been told are down (passed through or injected).
    pub fn os_keys(&self) -> &BTreeSet<u32> {
        &self.os
    }

    /// Whether a shortcut remap is currently active.
    pub fn shortcut_active(&self) -> bool {
        self.active.is_some()
    }

    /// Whether any remap can involve this physical key (cheap pre-filter).
    pub fn has_key_remap(&self, vk: u32) -> bool {
        self.compiled.keys.contains_key(&normalize_vk(vk))
    }

    /// Process one physical key event. `app` returns identifiers of the
    /// foreground application (bundle id / name / exe name); it is called
    /// lazily, only when an app-specific remap could match.
    pub fn process(
        &mut self,
        vk: u32,
        down: bool,
        app: &mut dyn FnMut() -> Vec<String>,
    ) -> Outcome {
        let vk = normalize_vk(vk);
        let first = down && !self.physical.contains(&vk);
        if down {
            self.physical.insert(vk);
        } else if !self.physical.remove(&vk) {
            // Key-up without a key-down we saw (held before we started): let it
            // through and forget it.
            self.logical.remove(&vk);
            self.os.remove(&vk);
            return Outcome::Pass;
        }

        let mut out = Vec::new();
        let single = if first {
            let s = self.single_for(vk);
            if let Some(s) = &s {
                self.single.insert(vk, s.clone());
            }
            s
        } else if down {
            self.single.get(&vk).cloned()
        } else {
            self.single.remove(&vk)
        };

        match single {
            None => self.logical_event(vk, down, &mut out, app),
            Some(Single::Map(t)) => self.logical_event(t, down, &mut out, app),
            Some(Single::Swallow) => {
                if first {
                    // Side actions fire once per press.
                    if let Some(Target::Run(r)) = self.compiled.keys.get(&vk) {
                        out.push(Action::Run(r.clone()));
                    } else if let Some(Target::OpenUri(u)) = self.compiled.keys.get(&vk) {
                        out.push(Action::OpenUri(u.clone()));
                    } else if let Some(Target::PastePlainText) = self.compiled.keys.get(&vk) {
                        out.push(Action::PastePlainText);
                    }
                }
            }
            Some(Single::Text(t)) => {
                if down {
                    out.push(Action::Text(t));
                }
            }
            Some(Single::Shortcut { target, pressed }) => {
                if down {
                    if first {
                        for &m in &pressed {
                            self.emit(&mut out, m, true);
                        }
                    }
                    self.emit(&mut out, target.key, true);
                } else {
                    if self.os.contains(&target.key) {
                        self.emit(&mut out, target.key, false);
                    }
                    for &m in pressed.iter().rev() {
                        if self.os.contains(&m) && !self.logical.contains(&m) {
                            self.emit(&mut out, m, false);
                        }
                    }
                }
            }
        }

        if out.len() == 1 && out[0] == (Action::Key { vk, down }) {
            Outcome::Pass
        } else {
            Outcome::Replace(out)
        }
    }

    /// Feed synthetic key-ups for physically-held keys that `still_down` says
    /// are no longer held (e.g. their release was missed during macOS secure
    /// input). Returns the actions to perform.
    pub fn release_missing(&mut self, still_down: impl Fn(u32) -> bool) -> Vec<Action> {
        let stale: Vec<u32> = self
            .physical
            .iter()
            .copied()
            .filter(|&k| !still_down(k))
            .collect();
        let mut out = Vec::new();
        for k in stale {
            match self.process(k, false, &mut Vec::new) {
                Outcome::Pass => out.push(Action::up(k)),
                Outcome::Replace(a) => out.extend(a),
            }
        }
        out
    }

    /// Bring the OS view back to the physical keyboard (used when stopping):
    /// release every key the OS thinks is down but the user isn't holding, and
    /// forget all remap state.
    pub fn reset(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        let mut stuck: Vec<u32> = self
            .os
            .iter()
            .copied()
            .filter(|k| !self.physical.contains(k))
            .collect();
        // Ordinary keys first, then modifiers in reverse shortcut order.
        stuck.sort_by_key(|&k| (is_modifier(k), std::cmp::Reverse(mod_order(k))));
        for k in stuck {
            self.emit(&mut out, k, false);
        }
        self.active = None;
        self.single.clear();
        self.logical = self.physical.clone();
        out
    }

    // -- layer 1 ------------------------------------------------------------

    fn single_for(&self, vk: u32) -> Option<Single> {
        let t = self.compiled.keys.get(&vk)?;
        Some(match t {
            Target::Disabled => Single::Swallow,
            Target::Key(k) => Single::Map(*k),
            Target::Text(s) => Single::Text(s.clone()),
            Target::Run(_) | Target::OpenUri(_) | Target::PastePlainText => Single::Swallow,
            Target::Shortcut(s) => {
                let pressed = resolve_mods(&s.mods, &[])
                    .into_iter()
                    .filter(|m| !self.os.contains(m))
                    .collect();
                Single::Shortcut {
                    target: *s,
                    pressed,
                }
            }
        })
    }

    // -- layer 2 ------------------------------------------------------------

    fn logical_event(
        &mut self,
        vk: u32,
        down: bool,
        out: &mut Vec<Action>,
        app: &mut dyn FnMut() -> Vec<String>,
    ) {
        let repeat = down && self.logical.contains(&vk);
        if down {
            self.logical.insert(vk);
        } else {
            self.logical.remove(&vk);
        }
        match self.active.take() {
            Some(a) => self.on_active(a, vk, down, repeat, out, app),
            None => self.on_idle(vk, down, out, app),
        }
    }

    fn on_idle(
        &mut self,
        vk: u32,
        down: bool,
        out: &mut Vec<Action>,
        app: &mut dyn FnMut() -> Vec<String>,
    ) {
        if down && !is_modifier(vk) {
            if let Some(i) = self.find_shortcut(vk, app) {
                self.activate(i, vk, out);
                return;
            }
        }
        if !down && !self.os.contains(&vk) {
            return; // its key-down was suppressed
        }
        self.emit(out, vk, down);
    }

    fn find_shortcut(&self, vk: u32, app: &mut dyn FnMut() -> Vec<String>) -> Option<usize> {
        let others_clear = self.logical.iter().all(|&k| k == vk || is_modifier(k));
        let candidates: Vec<usize> = self
            .compiled
            .shortcuts
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.from.key == vk
                    && e.from.mods.matches_exactly(|k| self.logical.contains(&k))
                    && (others_clear || matches!(e.to, Target::Key(_)))
            })
            .map(|(i, _)| i)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        if candidates
            .iter()
            .any(|&i| self.compiled.shortcuts[i].app.is_some())
        {
            let ids: Vec<String> = app().iter().map(|s| normalize_app(s)).collect();
            if let Some(&i) = candidates.iter().find(|&&i| {
                self.compiled.shortcuts[i]
                    .app
                    .as_ref()
                    .is_some_and(|a| ids.contains(a))
            }) {
                return Some(i);
            }
        }
        candidates
            .into_iter()
            .find(|&i| self.compiled.shortcuts[i].app.is_none())
    }

    fn held_mods(&self) -> Vec<u32> {
        self.logical
            .iter()
            .copied()
            .filter(|&k| is_modifier(k))
            .collect()
    }

    fn activate(&mut self, i: usize, vk: u32, out: &mut Vec<Action>) {
        let e = self.compiled.shortcuts[i].clone();
        let src_mods = self.held_mods();
        let mut a = Active {
            from: e.from,
            to: e.to,
            action: vk,
            src_mods,
            target_down: None,
        };
        // Fired by an auto-repeat of a key the OS already has down (the key was
        // held before the modifiers): release it, the remap consumes it now.
        if self.os.contains(&vk) {
            self.emit(out, vk, false);
        }
        match &a.to {
            Target::Shortcut(t) => {
                let desired = resolve_mods(&t.mods, &a.src_mods);
                self.sync_mods(out, &desired, Dummy::BeforeRelease);
                self.emit(out, t.key, true);
                a.target_down = Some(t.key);
            }
            Target::Key(t) => {
                let t = *t;
                let desired = if is_modifier(t) { vec![t] } else { vec![] };
                self.sync_mods(out, &desired, Dummy::BeforeRelease);
                if !is_modifier(t) {
                    self.emit(out, t, true);
                }
                a.target_down = Some(t);
            }
            Target::Text(s) => {
                self.sync_mods(out, &[], Dummy::BeforeRelease);
                out.push(Action::Text(s.clone()));
            }
            Target::PastePlainText => {
                self.sync_mods(out, &[], Dummy::BeforeRelease);
                out.push(Action::PastePlainText);
            }
            Target::Disabled => self.dummy_if_win_alt_held(out),
            Target::Run(r) => {
                self.dummy_if_win_alt_held(out);
                out.push(Action::Run(r.clone()));
            }
            Target::OpenUri(u) => {
                self.dummy_if_win_alt_held(out);
                out.push(Action::OpenUri(u.clone()));
            }
        }
        self.active = Some(a);
    }

    fn on_active(
        &mut self,
        mut a: Active,
        vk: u32,
        down: bool,
        repeat: bool,
        out: &mut Vec<Action>,
        app: &mut dyn FnMut() -> Vec<String>,
    ) {
        let src_family = mod_family(vk).is_some_and(|f| a.from.mods.get(f) != Side::None);
        if src_family {
            if !down {
                // A source modifier was released: end the remap and restore the
                // physical modifier state.
                self.deactivate(&a, out, None, Dummy::AfterPress);
            } else {
                // Repeat of a held source modifier (Windows auto-repeats
                // modifiers) or the other side of an either-side modifier:
                // forward only if the OS already has it down.
                if self.os.contains(&vk) {
                    self.emit(out, vk, true);
                }
                self.active = Some(a);
            }
            return;
        }

        if vk == a.action {
            if down {
                match &a.to {
                    Target::Shortcut(t) => {
                        self.emit(out, t.key, true);
                        a.target_down = Some(t.key);
                    }
                    Target::Key(t) => {
                        self.emit(out, *t, true);
                        a.target_down = Some(*t);
                    }
                    Target::Text(s) => out.push(Action::Text(s.clone())),
                    Target::PastePlainText if !repeat => out.push(Action::PastePlainText),
                    Target::Run(r) if !repeat => out.push(Action::Run(r.clone())),
                    Target::OpenUri(u) if !repeat => out.push(Action::OpenUri(u.clone())),
                    _ => {}
                }
            } else if let Some(t) = a.target_down.take() {
                // Keep a target modifier the user also holds (via another key).
                if self.os.contains(&t) && !(is_modifier(t) && self.logical.contains(&t)) {
                    self.emit(out, t, false);
                }
            }
            self.active = Some(a);
            return;
        }

        if down {
            if matches!(a.to, Target::Key(_)) && a.target_down.is_some() {
                // Shortcut -> key held down, used together with another key
                // (Alt+D -> Win, then A gives Win+A).
                self.emit(out, vk, true);
                self.active = Some(a);
                return;
            }
            // Any other key ends the remap; then handle the key afresh.
            self.deactivate(&a, out, Some(vk), Dummy::None);
            self.on_idle(vk, down, out, app);
            return;
        }

        if self.os.contains(&vk) {
            self.emit(out, vk, false);
        }
        self.active = Some(a);
    }

    fn deactivate(&mut self, a: &Active, out: &mut Vec<Action>, except: Option<u32>, dummy: Dummy) {
        if let Some(t) = a.target_down {
            if !is_modifier(t) && self.os.contains(&t) {
                self.emit(out, t, false);
            }
        }
        let desired: Vec<u32> = self
            .held_mods()
            .into_iter()
            .filter(|&k| Some(k) != except)
            .collect();
        self.sync_mods(out, &desired, dummy);
    }

    /// Make the OS-level modifier set equal `desired`.
    fn sync_mods(&mut self, out: &mut Vec<Action>, desired: &[u32], dummy: Dummy) {
        let current: Vec<u32> = self
            .os
            .iter()
            .copied()
            .filter(|&k| is_modifier(k))
            .collect();
        let mut release: Vec<u32> = current
            .iter()
            .copied()
            .filter(|k| !desired.contains(k))
            .collect();
        let mut press: Vec<u32> = desired
            .iter()
            .copied()
            .filter(|k| !self.os.contains(k))
            .collect();
        // Press in shortcut order (Win, Ctrl, Alt, Shift), release in reverse.
        release.sort_by_key(|&k| mod_order(k));
        press.sort_by_key(|&k| mod_order(k));
        if dummy == Dummy::BeforeRelease && release.iter().any(|&k| is_win_or_alt(k)) {
            self.dummy(out);
        }
        for &k in release.iter().rev() {
            self.emit(out, k, false);
        }
        for &k in &press {
            self.emit(out, k, true);
        }
        if dummy == Dummy::AfterPress && press.iter().any(|&k| is_win_or_alt(k)) {
            self.dummy(out);
        }
    }

    fn dummy_if_win_alt_held(&mut self, out: &mut Vec<Action>) {
        if self.os.iter().any(|&k| is_win_or_alt(k)) {
            self.dummy(out);
        }
    }

    fn dummy(&self, out: &mut Vec<Action>) {
        if self.opts.dummy_key {
            out.push(Action::down(VK_DUMMY));
            out.push(Action::up(VK_DUMMY));
        }
    }

    fn emit(&mut self, out: &mut Vec<Action>, vk: u32, down: bool) {
        if vk != VK_DUMMY {
            if down {
                self.os.insert(vk);
            } else {
                self.os.remove(&vk);
            }
        }
        out.push(Action::Key { vk, down });
    }
}

#[cfg(test)]
mod tests;
