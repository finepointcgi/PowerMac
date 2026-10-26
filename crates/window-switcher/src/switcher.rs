//! Keyboard / mouse state machine of the switcher (platform independent).
//!
//! The platform feeds key downs/ups, modifier changes, mouse hover/clicks
//! and timer ticks; the machine answers whether to swallow the event and
//! which [`Effect`]s to perform. Timing comes in as `Instant`s so it can be
//! tested without sleeping.
//!
//! Lifecycle:
//! 1. Idle. A trigger (e.g. Option+Tab) opens the switcher: the event is
//!    swallowed, [`Effect::Open`] asks the platform for the window list
//!    (it calls [`Switcher::set_count`]) and the second entry — the previous
//!    window — is selected (the last one with Shift).
//! 2. Active, not yet shown. If the held modifier is released before
//!    `show_delay` ("fast switch"), the selection is committed without ever
//!    showing the overlay. Otherwise [`Switcher::tick`] emits
//!    [`Effect::Show`] once the delay has passed.
//! 3. Active and shown. Tab / Shift+Tab / arrows / hover change the
//!    selection; releasing the modifier, Return, Space or a click commit;
//!    Esc cancels; Q / W / M / H / F act on the selected window. Every other
//!    key is swallowed so nothing leaks into the focused app.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use pm_input::vk;

use crate::config::{Hotkey, Mods};
use crate::layout::Dir;
use crate::model::Mode;

/// What to do to the selected window (AltTab's shortcuts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowAction {
    /// Q: quit the app.
    QuitApp,
    /// W: close the window.
    Close,
    /// M: minimize / restore the window.
    ToggleMinimize,
    /// H: hide / unhide the app.
    ToggleHide,
    /// F: toggle full screen.
    ToggleFullScreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Snapshot the window list for `mode` and call [`Switcher::set_count`].
    Open(Mode),
    /// Show the overlay (the list is already loaded).
    Show,
    /// Highlight entry `i`.
    Select(usize),
    /// Hide the overlay and focus entry `i`.
    Commit(usize),
    /// Hide the overlay without switching.
    Cancel,
    /// Perform an action on entry `i`; the switcher stays open.
    Act(WindowAction, usize),
}

/// Result of feeding an input event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    /// Don't deliver the event to the focused app.
    pub swallow: bool,
    pub effects: Vec<Effect>,
}

impl Outcome {
    fn pass() -> Self {
        Self::default()
    }
    fn swallow(effects: Vec<Effect>) -> Self {
        Self { swallow: true, effects }
    }
}

/// The shortcuts the machine reacts to.
#[derive(Debug, Clone, PartialEq)]
pub struct Keys {
    pub trigger: Hotkey,
    pub app_trigger: Hotkey,
    /// Cmd+Tab (macOS) / Alt+Tab (Windows) when `replace_cmd_tab` is on.
    pub system_trigger: Option<Hotkey>,
    pub show_delay: Duration,
}

impl Keys {
    fn classify(&self, code: u32, mods: Mods) -> Option<(Mode, Hotkey)> {
        if self.trigger.matches(code, mods) {
            return Some((Mode::AllWindows, self.trigger));
        }
        if let Some(s) = self.system_trigger {
            if s.matches(code, mods) {
                return Some((Mode::AllWindows, s));
            }
        }
        if self.app_trigger.matches(code, mods) {
            return Some((Mode::CurrentApp, self.app_trigger));
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Idle,
    Active {
        mode: Mode,
        /// Modifiers whose release commits (empty when sticky).
        held: Mods,
        /// Opened from the API / without modifiers: stays until Return/Esc/click.
        sticky: bool,
        started: Instant,
        shown: bool,
        /// The trigger was pressed with Shift (start from the end).
        backwards: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Switcher {
    keys: Keys,
    phase: Phase,
    count: usize,
    selected: usize,
    /// Keys whose key-down we swallowed: swallow their key-up too.
    swallowed: HashSet<u32>,
}

impl Switcher {
    pub fn new(keys: Keys) -> Self {
        Self { keys, phase: Phase::Idle, count: 0, selected: 0, swallowed: HashSet::new() }
    }

    pub fn set_keys(&mut self, keys: Keys) {
        self.keys = keys;
    }

    pub fn keys(&self) -> &Keys {
        &self.keys
    }

    pub fn is_active(&self) -> bool {
        matches!(self.phase, Phase::Active { .. })
    }

    pub fn is_shown(&self) -> bool {
        matches!(self.phase, Phase::Active { shown: true, .. })
    }

    pub fn mode(&self) -> Option<Mode> {
        match self.phase {
            Phase::Active { mode, .. } => Some(mode),
            Phase::Idle => None,
        }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// When the overlay should appear (for scheduling a [`Switcher::tick`]).
    pub fn show_deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::Active { shown: false, started, .. } => Some(started + self.keys.show_delay),
            _ => None,
        }
    }

    /// The platform loaded the list after [`Effect::Open`] (or refreshed it
    /// while open). Picks the initial selection on the first call.
    pub fn set_count(&mut self, n: usize) -> Vec<Effect> {
        let first = self.count == usize::MAX;
        self.count = n;
        if n == 0 {
            self.selected = 0;
            return vec![];
        }
        if first {
            let backwards = matches!(self.phase, Phase::Active { backwards: true, .. });
            // Entry 0 is the current window; start at the previous one.
            self.selected = if n == 1 {
                0
            } else if backwards {
                n - 1
            } else {
                1
            };
        } else {
            self.selected = self.selected.min(n - 1);
        }
        vec![Effect::Select(self.selected)]
    }

    fn open(&mut self, mode: Mode, held: Mods, sticky: bool, backwards: bool, now: Instant, show_now: bool) -> Vec<Effect> {
        self.phase = Phase::Active { mode, held, sticky, started: now, shown: show_now, backwards };
        // Sentinel: the next set_count is the initial one.
        self.count = usize::MAX;
        self.selected = 0;
        let mut fx = vec![Effect::Open(mode)];
        if show_now {
            fx.push(Effect::Show);
        }
        fx
    }

    /// Open from the API (tray item, etc.): shown immediately, stays open
    /// until Return / click / Esc.
    pub fn open_sticky(&mut self, mode: Mode, now: Instant) -> Vec<Effect> {
        if self.is_active() {
            return vec![];
        }
        self.open(mode, Mods::NONE, true, false, now, true)
    }

    fn step(&mut self, delta: isize) -> Vec<Effect> {
        let n = self.count_or_zero();
        if n == 0 {
            return vec![];
        }
        let i = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        self.select(i)
    }

    fn count_or_zero(&self) -> usize {
        if self.count == usize::MAX {
            0
        } else {
            self.count
        }
    }

    /// Select entry `i` (e.g. mouse hover or arrow keys via the layout).
    pub fn select(&mut self, i: usize) -> Vec<Effect> {
        if !self.is_active() || i >= self.count_or_zero() {
            return vec![];
        }
        if i == self.selected {
            return vec![];
        }
        self.selected = i;
        vec![Effect::Select(i)]
    }

    fn finish(&mut self, effect: Effect) -> Vec<Effect> {
        self.phase = Phase::Idle;
        let valid = self.count_or_zero() > 0;
        self.count = 0;
        match effect {
            Effect::Commit(_) if !valid => vec![Effect::Cancel],
            e => vec![e],
        }
    }

    /// Commit the current selection (Return, click, programmatic).
    pub fn commit(&mut self) -> Vec<Effect> {
        if !self.is_active() {
            return vec![];
        }
        let i = self.selected;
        self.finish(Effect::Commit(i))
    }

    /// Close without switching.
    pub fn cancel(&mut self) -> Vec<Effect> {
        if !self.is_active() {
            return vec![];
        }
        self.finish(Effect::Cancel)
    }

    /// A key went down. `code` is a Windows VK; `mods` the modifiers held
    /// (excluding the key itself); `nav` maps arrow directions to the
    /// target index (from the layout).
    pub fn key_down(&mut self, code: u32, mods: Mods, now: Instant, nav: impl Fn(usize, Dir) -> usize) -> Outcome {
        if vk::is_modifier_vk(code) {
            return Outcome::pass();
        }
        match self.phase.clone() {
            Phase::Idle => match self.keys.classify(code, mods) {
                Some((mode, hk)) => {
                    let mut held = hk.held();
                    let sticky = held.is_empty();
                    // Shift in the hotkey itself doesn't have to stay held.
                    held.shift = false;
                    let backwards = mods.shift && !hk.shift;
                    self.swallowed.insert(code);
                    Outcome::swallow(self.open(mode, held, sticky, backwards, now, sticky))
                }
                None => Outcome::pass(),
            },
            Phase::Active { .. } => {
                self.swallowed.insert(code);
                let fx = if let Some((_, hk)) = self.keys.classify(code, mods) {
                    if mods.shift && !hk.shift {
                        self.step(-1)
                    } else {
                        self.step(1)
                    }
                } else {
                    match code {
                        vk::VK_ESCAPE => self.cancel(),
                        vk::VK_RETURN | vk::VK_SPACE => self.commit(),
                        vk::VK_LEFT | vk::VK_RIGHT | vk::VK_UP | vk::VK_DOWN => {
                            let dir = match code {
                                vk::VK_LEFT => Dir::Left,
                                vk::VK_RIGHT => Dir::Right,
                                vk::VK_UP => Dir::Up,
                                _ => Dir::Down,
                            };
                            let n = self.count_or_zero();
                            if n == 0 {
                                vec![]
                            } else {
                                let to = nav(self.selected, dir).min(n - 1);
                                self.select(to)
                            }
                        }
                        // Tab without the modifier, e.g. in sticky mode.
                        vk::VK_TAB => self.step(if mods.shift { -1 } else { 1 }),
                        c => match action_for(c) {
                            Some(a) if self.count_or_zero() > 0 => vec![Effect::Act(a, self.selected)],
                            _ => vec![],
                        },
                    }
                };
                Outcome::swallow(fx)
            }
        }
    }

    /// A key went up: swallow it if we swallowed its key-down.
    pub fn key_up(&mut self, code: u32) -> Outcome {
        if self.swallowed.remove(&code) {
            Outcome::swallow(vec![])
        } else {
            Outcome::pass()
        }
    }

    /// The set of held modifiers changed. Releasing a held trigger modifier
    /// commits — before the overlay is shown this is the "fast switch".
    pub fn modifiers_changed(&mut self, mods: Mods) -> Vec<Effect> {
        match self.phase {
            Phase::Active { held, sticky: false, .. } if !mods.contains(held) => self.commit(),
            _ => vec![],
        }
    }

    /// Timer: show the overlay once the delay has passed.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        if let Phase::Active { shown: shown @ false, started, .. } = &mut self.phase {
            if now >= *started + self.keys.show_delay {
                *shown = true;
                return vec![Effect::Show];
            }
        }
        vec![]
    }

    /// Mouse moved over entry `i` (only while shown).
    pub fn hover(&mut self, i: Option<usize>) -> Vec<Effect> {
        match i {
            Some(i) if self.is_shown() => self.select(i),
            _ => vec![],
        }
    }

    /// Mouse click while shown: on a tile commits it, outside cancels.
    /// Returns whether to swallow the click.
    pub fn click(&mut self, i: Option<usize>) -> Outcome {
        if !self.is_shown() {
            return Outcome::pass();
        }
        match i {
            Some(i) if i < self.count_or_zero() => {
                self.selected = i;
                Outcome::swallow(self.commit())
            }
            Some(_) => Outcome::swallow(vec![]),
            None => Outcome { swallow: false, effects: self.cancel() },
        }
    }
}

fn action_for(code: u32) -> Option<WindowAction> {
    Some(match code {
        0x51 => WindowAction::QuitApp,          // Q
        0x57 => WindowAction::Close,            // W
        0x4D => WindowAction::ToggleMinimize,   // M
        0x48 => WindowAction::ToggleHide,       // H
        0x46 => WindowAction::ToggleFullScreen, // F
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPT: Mods = Mods::new(false, false, true, false);
    const OPT_SHIFT: Mods = Mods::new(false, false, true, true);
    const CMD: Mods = Mods::new(true, false, false, false);
    const DELAY: Duration = Duration::from_millis(90);

    fn keys() -> Keys {
        Keys {
            trigger: Hotkey::new(false, false, true, false, vk::VK_TAB),
            app_trigger: Hotkey::new(false, false, true, false, vk::VK_OEM_3),
            system_trigger: None,
            show_delay: DELAY,
        }
    }

    fn nav(i: usize, d: Dir) -> usize {
        match d {
            Dir::Right | Dir::Down => i + 1,
            Dir::Left | Dir::Up => i.saturating_sub(1),
        }
    }

    /// Press the trigger and load `n` windows.
    fn open(s: &mut Switcher, t0: Instant, n: usize, mods: Mods) -> Vec<Effect> {
        let o = s.key_down(vk::VK_TAB, mods, t0, nav);
        assert!(o.swallow);
        assert_eq!(o.effects, vec![Effect::Open(Mode::AllWindows)]);
        s.set_count(n)
    }

    #[test]
    fn fast_switch_commits_previous_without_showing() {
        let mut s = Switcher::new(keys());
        let t0 = Instant::now();
        assert_eq!(open(&mut s, t0, 4, OPT), vec![Effect::Select(1)]);
        assert!(s.is_active() && !s.is_shown());
        assert_eq!(s.show_deadline(), Some(t0 + DELAY));
        // A tick before the deadline does nothing.
        assert!(s.tick(t0 + Duration::from_millis(40)).is_empty());
        // Tab key-up is swallowed too.
        assert!(s.key_up(vk::VK_TAB).swallow);
        // Option released at 60 ms: switch to entry 1, overlay never shown.
        assert_eq!(s.modifiers_changed(Mods::NONE), vec![Effect::Commit(1)]);
        assert!(!s.is_active());
        assert!(s.tick(t0 + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn hold_shows_after_delay_then_cycles_and_commits_on_release() {
        let mut s = Switcher::new(keys());
        let t0 = Instant::now();
        open(&mut s, t0, 3, OPT);
        assert_eq!(s.tick(t0 + DELAY), vec![Effect::Show]);
        assert!(s.is_shown());
        assert!(s.tick(t0 + DELAY * 2).is_empty(), "Show only once");
        // Tab again → 2, again → wraps to 0, Shift+Tab → back to 2.
        assert_eq!(s.key_down(vk::VK_TAB, OPT, t0, nav).effects, vec![Effect::Select(2)]);
        assert_eq!(s.key_down(vk::VK_TAB, OPT, t0, nav).effects, vec![Effect::Select(0)]);
        assert_eq!(s.key_down(vk::VK_TAB, OPT_SHIFT, t0, nav).effects, vec![Effect::Select(2)]);
        // Pressing / releasing Shift alone keeps it open.
        assert!(s.modifiers_changed(OPT_SHIFT).is_empty());
        assert!(s.modifiers_changed(OPT).is_empty());
        assert_eq!(s.modifiers_changed(Mods::NONE), vec![Effect::Commit(2)]);
    }

    #[test]
    fn shift_trigger_starts_from_the_end() {
        let mut s = Switcher::new(keys());
        assert_eq!(open(&mut s, Instant::now(), 5, OPT_SHIFT), vec![Effect::Select(4)]);
    }

    #[test]
    fn escape_cancels_and_is_swallowed() {
        let mut s = Switcher::new(keys());
        let t0 = Instant::now();
        open(&mut s, t0, 3, OPT);
        let o = s.key_down(vk::VK_ESCAPE, OPT, t0, nav);
        assert!(o.swallow);
        assert_eq!(o.effects, vec![Effect::Cancel]);
        assert!(!s.is_active());
        assert!(s.key_up(vk::VK_ESCAPE).swallow);
        // Releasing Option afterwards does nothing.
        assert!(s.modifiers_changed(Mods::NONE).is_empty());
        // Unrelated keys pass when idle.
        assert!(!s.key_down(0x41, Mods::NONE, t0, nav).swallow);
        assert!(!s.key_up(0x41).swallow);
    }

    #[test]
    fn other_keys_are_swallowed_while_active() {
        let mut s = Switcher::new(keys());
        let t0 = Instant::now();
        open(&mut s, t0, 3, OPT);
        let o = s.key_down(0x41, OPT, t0, nav); // A
        assert!(o.swallow && o.effects.is_empty());
        // Modifier key downs are never swallowed.
        assert!(!s.key_down(vk::VK_LSHIFT, OPT, t0, nav).swallow);
    }

    #[test]
    fn arrows_return_and_actions() {
        let mut s = Switcher::new(keys());
        let t0 = Instant::now();
        open(&mut s, t0, 4, OPT);
        assert_eq!(s.key_down(vk::VK_RIGHT, OPT, t0, nav).effects, vec![Effect::Select(2)]);
        assert_eq!(s.key_down(vk::VK_DOWN, OPT, t0, |_, _| 99).effects, vec![Effect::Select(3)], "clamped");
        assert_eq!(s.key_down(vk::VK_LEFT, OPT, t0, nav).effects, vec![Effect::Select(2)]);
        assert_eq!(s.key_down(0x57, OPT, t0, nav).effects, vec![Effect::Act(WindowAction::Close, 2)]);
        assert_eq!(s.key_down(0x4D, OPT, t0, nav).effects, vec![Effect::Act(WindowAction::ToggleMinimize, 2)]);
        assert_eq!(s.key_down(0x51, OPT, t0, nav).effects, vec![Effect::Act(WindowAction::QuitApp, 2)]);
        assert_eq!(s.key_down(0x48, OPT, t0, nav).effects, vec![Effect::Act(WindowAction::ToggleHide, 2)]);
        assert_eq!(s.key_down(0x46, OPT, t0, nav).effects, vec![Effect::Act(WindowAction::ToggleFullScreen, 2)]);
        assert!(s.is_active(), "actions keep the switcher open");
        // The window closed: list refreshed with fewer entries keeps a valid selection.
        assert_eq!(s.set_count(2), vec![Effect::Select(1)]);
        assert_eq!(s.key_down(vk::VK_RETURN, OPT, t0, nav).effects, vec![Effect::Commit(1)]);
        assert!(!s.is_active());
    }

    #[test]
    fn app_windows_trigger_and_cmd_tab_replacement() {
        let mut k = keys();
        k.system_trigger = Some(Hotkey::new(true, false, false, false, vk::VK_TAB));
        let mut s = Switcher::new(k);
        let t0 = Instant::now();
        let o = s.key_down(vk::VK_OEM_3, OPT, t0, nav);
        assert_eq!(o.effects, vec![Effect::Open(Mode::CurrentApp)]);
        assert_eq!(s.mode(), Some(Mode::CurrentApp));
        s.set_count(3);
        // ` again cycles.
        assert_eq!(s.key_down(vk::VK_OEM_3, OPT, t0, nav).effects, vec![Effect::Select(2)]);
        s.modifiers_changed(Mods::NONE);
        // Cmd+Tab opens the all-windows switcher; releasing Cmd commits.
        let o = s.key_down(vk::VK_TAB, CMD, t0, nav);
        assert!(o.swallow);
        assert_eq!(o.effects, vec![Effect::Open(Mode::AllWindows)]);
        s.set_count(2);
        assert_eq!(s.modifiers_changed(Mods::NONE), vec![Effect::Commit(1)]);
        // Without replacement Cmd+Tab passes through.
        let mut plain = Switcher::new(keys());
        assert!(!plain.key_down(vk::VK_TAB, CMD, t0, nav).swallow);
    }

    #[test]
    fn single_and_empty_lists() {
        let mut s = Switcher::new(keys());
        let t0 = Instant::now();
        assert_eq!(open(&mut s, t0, 1, OPT), vec![Effect::Select(0)]);
        assert_eq!(s.modifiers_changed(Mods::NONE), vec![Effect::Commit(0)]);
        assert!(open(&mut s, t0, 0, OPT).is_empty());
        assert!(s.key_down(vk::VK_TAB, OPT, t0, nav).effects.is_empty());
        assert!(s.key_down(0x57, OPT, t0, nav).effects.is_empty(), "no action without windows");
        assert_eq!(s.modifiers_changed(Mods::NONE), vec![Effect::Cancel]);
        // Released before the list arrived (count never set) → cancel.
        s.key_down(vk::VK_TAB, OPT, t0, nav);
        assert_eq!(s.modifiers_changed(Mods::NONE), vec![Effect::Cancel]);
    }

    #[test]
    fn mouse_hover_and_click() {
        let mut s = Switcher::new(keys());
        let t0 = Instant::now();
        open(&mut s, t0, 3, OPT);
        assert!(s.hover(Some(2)).is_empty(), "no hover before shown");
        assert!(!s.click(Some(2)).swallow);
        s.tick(t0 + DELAY);
        assert_eq!(s.hover(Some(2)), vec![Effect::Select(2)]);
        assert!(s.hover(None).is_empty());
        let o = s.click(Some(0));
        assert!(o.swallow);
        assert_eq!(o.effects, vec![Effect::Commit(0)]);
        // Click outside cancels and passes the click on.
        open(&mut s, t0, 3, OPT);
        s.tick(t0 + DELAY);
        let o = s.click(None);
        assert!(!o.swallow);
        assert_eq!(o.effects, vec![Effect::Cancel]);
    }

    #[test]
    fn sticky_open_ignores_modifier_release() {
        let mut s = Switcher::new(keys());
        let t0 = Instant::now();
        assert_eq!(s.open_sticky(Mode::AllWindows, t0), vec![Effect::Open(Mode::AllWindows), Effect::Show]);
        assert_eq!(s.set_count(3), vec![Effect::Select(1)]);
        assert!(s.modifiers_changed(Mods::NONE).is_empty());
        // Plain Tab cycles in sticky mode.
        assert_eq!(s.key_down(vk::VK_TAB, Mods::NONE, t0, nav).effects, vec![Effect::Select(2)]);
        assert_eq!(s.key_down(vk::VK_SPACE, Mods::NONE, t0, nav).effects, vec![Effect::Commit(2)]);
        assert!(s.open_sticky(Mode::AllWindows, t0).len() == 2);
        assert!(s.open_sticky(Mode::AllWindows, t0).is_empty(), "already open");
    }

    #[test]
    fn zero_delay_shows_on_first_tick() {
        let mut k = keys();
        k.show_delay = Duration::ZERO;
        let mut s = Switcher::new(k);
        let t0 = Instant::now();
        open(&mut s, t0, 3, OPT);
        assert_eq!(s.tick(t0), vec![Effect::Show]);
    }
}
