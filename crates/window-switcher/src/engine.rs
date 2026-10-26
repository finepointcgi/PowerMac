//! Glue between the key state machine, the window list and the layout.
//! Platform independent; the platform runtimes hold one [`Engine`] behind a
//! mutex and perform the effects it returns.

use std::time::{Duration, Instant};

use crate::config::{Config, Hotkey, Mods};
use crate::layout::{self, Layout, LayoutParams};
use crate::model::{Mode, RectF, WindowId, WindowInfo};
use crate::switcher::{Effect, Keys, Outcome, Switcher};

/// What the platform hands over when the switcher opens.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Filtered, ordered (most recent first) windows.
    pub items: Vec<WindowInfo>,
    /// The screen to show the switcher on (global coordinates).
    pub screen: RectF,
    /// Draw app icons instead of thumbnails.
    pub icon_mode: bool,
}

pub struct Engine {
    cfg: Config,
    switcher: Switcher,
    items: Vec<WindowInfo>,
    layout: Layout,
    screen: RectF,
    icon_mode: bool,
}

pub fn keys_for(cfg: &Config) -> Keys {
    Keys {
        trigger: if cfg.enabled { cfg.trigger } else { Hotkey::none() },
        app_trigger: if cfg.enabled { cfg.app_windows_trigger } else { Hotkey::none() },
        system_trigger: cfg.replaces_system_switcher().then(Hotkey::system_switcher),
        system_app_trigger: cfg.replaces_cmd_backtick().then(Hotkey::system_app_switcher),
        show_delay: Duration::from_millis(cfg.show_delay_ms.min(2000) as u64),
    }
}

impl Engine {
    pub fn new(cfg: Config) -> Self {
        Self {
            switcher: Switcher::new(keys_for(&cfg)),
            cfg,
            items: vec![],
            layout: layout::compute(&[], RectF::default(), &LayoutParams::THUMBNAILS),
            screen: RectF::default(),
            icon_mode: false,
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Apply new settings; closes the switcher if it was open.
    pub fn set_config(&mut self, cfg: Config) -> Vec<Effect> {
        let fx = if cfg != self.cfg { self.switcher.cancel() } else { vec![] };
        self.switcher.set_keys(keys_for(&cfg));
        self.cfg = cfg;
        fx
    }

    pub fn switcher(&self) -> &Switcher {
        &self.switcher
    }

    pub fn items(&self) -> &[WindowInfo] {
        &self.items
    }

    pub fn item(&self, i: usize) -> Option<&WindowInfo> {
        self.items.get(i)
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The screen the switcher is shown on (global coordinates).
    pub fn screen(&self) -> RectF {
        self.screen
    }

    /// The selected window, if the switcher is open and has windows.
    pub fn selected_item(&self) -> Option<&WindowInfo> {
        if self.is_active() {
            self.items.get(self.selected())
        } else {
            None
        }
    }

    pub fn icon_mode(&self) -> bool {
        self.icon_mode
    }

    pub fn selected(&self) -> usize {
        self.switcher.selected()
    }

    pub fn is_active(&self) -> bool {
        self.switcher.is_active()
    }

    pub fn is_shown(&self) -> bool {
        self.switcher.is_shown()
    }

    fn relayout(&mut self) {
        let aspects: Vec<f64> = self.items.iter().map(|w| if self.icon_mode { 1.0 } else { w.aspect() }).collect();
        let p = if self.icon_mode { LayoutParams::ICONS } else { LayoutParams::THUMBNAILS };
        self.layout = layout::compute(&aspects, self.screen, &p);
        // Items that didn't fit can't be selected.
        self.items.truncate(self.layout.tiles.len().max(if aspects.is_empty() { 0 } else { 1 }));
    }

    /// Resolve `Open` effects by loading a snapshot; returns the effects
    /// left for the platform.
    fn resolve(&mut self, fx: Vec<Effect>, open: &mut dyn FnMut(Mode) -> Snapshot) -> Vec<Effect> {
        let mut out = Vec::new();
        for e in fx {
            match e {
                Effect::Open(mode) => {
                    let snap = open(mode);
                    self.items = snap.items;
                    self.screen = snap.screen;
                    self.icon_mode = snap.icon_mode;
                    self.relayout();
                    out.push(Effect::Open(mode));
                    out.extend(self.switcher.set_count(self.items.len()));
                }
                e => out.push(e),
            }
        }
        out
    }

    pub fn key_down(&mut self, code: u32, mods: Mods, now: Instant, open: &mut dyn FnMut(Mode) -> Snapshot) -> Outcome {
        let layout = self.layout.clone();
        let o = self.switcher.key_down(code, mods, now, |i, d| layout.neighbor(i, d));
        Outcome { swallow: o.swallow, effects: self.resolve(o.effects, open) }
    }

    pub fn key_up(&mut self, code: u32) -> Outcome {
        self.switcher.key_up(code)
    }

    pub fn modifiers_changed(&mut self, mods: Mods) -> Vec<Effect> {
        self.switcher.modifiers_changed(mods)
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        self.switcher.tick(now)
    }

    pub fn show_deadline(&self) -> Option<Instant> {
        self.switcher.show_deadline()
    }

    pub fn open_sticky(&mut self, mode: Mode, now: Instant, open: &mut dyn FnMut(Mode) -> Snapshot) -> Vec<Effect> {
        let fx = self.switcher.open_sticky(mode, now);
        self.resolve(fx, open)
    }

    pub fn cancel(&mut self) -> Vec<Effect> {
        self.switcher.cancel()
    }

    /// Pointer moved (global coordinates).
    pub fn hover(&mut self, x: f64, y: f64) -> Vec<Effect> {
        let i = self.layout.hit_global(x, y);
        self.switcher.hover(i)
    }

    /// Click (global coordinates).
    pub fn click(&mut self, x: f64, y: f64) -> Outcome {
        let i = if self.layout.panel.contains(x, y) {
            // Inside the panel but between tiles: swallow, do nothing.
            Some(self.layout.hit_global(x, y).unwrap_or(usize::MAX))
        } else {
            None
        };
        self.switcher.click(i)
    }

    /// A fresher window list arrived while the switcher is open (e.g. after
    /// closing a window). Keeps the on-screen order stable, drops windows
    /// that went away, appends new ones, keeps the selected window selected.
    /// Returns `Select` if the selection index changed, and whether the
    /// list changed (needs a redraw).
    pub fn refresh(&mut self, fresh: Vec<WindowInfo>) -> (bool, Vec<Effect>) {
        if !self.switcher.is_active() {
            return (false, vec![]);
        }
        let selected_id = self.items.get(self.selected()).map(|w| w.id);
        let merged = merge_snapshot(&self.items, fresh);
        if merged == self.items {
            return (false, vec![]);
        }
        self.items = merged;
        self.relayout();
        let mut fx = self.switcher.set_count(self.items.len());
        if let Some(i) = selected_id.and_then(|id| self.items.iter().position(|w| w.id == id)) {
            fx = self.switcher.select(i);
            if fx.is_empty() {
                fx = vec![Effect::Select(i)];
            }
        }
        (true, fx)
    }

    pub fn index_of(&self, id: WindowId) -> Option<usize> {
        self.items.iter().position(|w| w.id == id)
    }
}

/// Merge a fresh list into the displayed one: displayed order wins, entries
/// are updated in place, vanished ones removed, new ones appended (in the
/// fresh list's order).
pub fn merge_snapshot(shown: &[WindowInfo], fresh: Vec<WindowInfo>) -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = shown.iter().filter_map(|old| fresh.iter().find(|f| f.id == old.id).cloned()).collect();
    for f in fresh {
        if !out.iter().any(|w| w.id == f.id) {
            out.push(f);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_input::vk;

    fn w(id: WindowId, title: &str) -> WindowInfo {
        WindowInfo { id, pid: 1, title: title.into(), frame: RectF::new(0.0, 0.0, 800.0, 500.0), ..Default::default() }
    }

    const OPT: Mods = Mods::new(false, false, true, false);
    const SCREEN: RectF = RectF::new(0.0, 0.0, 1440.0, 900.0);

    #[test]
    fn merge_keeps_order_updates_and_appends() {
        let shown = vec![w(1, "a"), w(2, "b"), w(3, "c")];
        let fresh = vec![w(4, "d"), w(3, "c2"), w(1, "a")];
        let m = merge_snapshot(&shown, fresh);
        assert_eq!(m.iter().map(|w| w.id).collect::<Vec<_>>(), vec![1, 3, 4]);
        assert_eq!(m[1].title, "c2");
    }

    #[test]
    fn open_loads_snapshot_and_lays_out() {
        let mut e = Engine::new(Config::default());
        let t0 = Instant::now();
        let mut calls = 0;
        let o = e.key_down(vk::VK_TAB, OPT, t0, &mut |m| {
            calls += 1;
            assert_eq!(m, Mode::AllWindows);
            Snapshot { items: vec![w(10, "x"), w(11, "y"), w(12, "z")], screen: SCREEN, icon_mode: false }
        });
        assert_eq!(calls, 1);
        assert!(o.swallow);
        assert_eq!(o.effects, vec![Effect::Open(Mode::AllWindows), Effect::Select(1)]);
        assert_eq!(e.layout().tiles.len(), 3);
        assert!(e.layout().panel.w > 0.0);
        // Hover the third tile once shown.
        assert_eq!(e.tick(t0 + Duration::from_secs(1)), vec![Effect::Show]);
        let t = e.layout().tiles[2].frame;
        let p = e.layout().panel;
        assert_eq!(e.hover(p.x + t.x + 4.0, p.y + t.y + 4.0), vec![Effect::Select(2)]);
        // Window 12 closed → refresh keeps a valid selection on the same window
        // when it still exists, otherwise clamps.
        let (changed, fx) = e.refresh(vec![w(10, "x"), w(11, "y")]);
        assert!(changed);
        assert_eq!(fx, vec![Effect::Select(1)]);
        assert_eq!(e.items().len(), 2);
        let (changed, _) = e.refresh(vec![w(10, "x"), w(11, "y")]);
        assert!(!changed);
        // A click between tiles inside the panel is swallowed without effect;
        // outside cancels.
        let p = e.layout().panel;
        let o = e.click(p.x + 1.0, p.y + 1.0);
        assert!(o.swallow && o.effects.is_empty());
        let o = e.click(p.x - 50.0, p.y);
        assert_eq!(o.effects, vec![Effect::Cancel]);
    }

    #[test]
    fn disabled_config_ignores_triggers() {
        let mut e = Engine::new(Config { enabled: false, ..Default::default() });
        let o = e.key_down(vk::VK_TAB, OPT, Instant::now(), &mut |_| unreachable!());
        assert!(!o.swallow);
    }

    #[test]
    fn config_change_cancels_open_switcher() {
        let mut e = Engine::new(Config::default());
        e.key_down(vk::VK_TAB, OPT, Instant::now(), &mut |_| Snapshot { items: vec![w(1, "a")], screen: SCREEN, icon_mode: true });
        assert!(e.is_active());
        assert!(e.icon_mode());
        assert_eq!(e.set_config(Config { thumbnails: false, ..Default::default() }), vec![Effect::Cancel]);
        assert!(!e.is_active());
    }
}
