//! Window list model: what a window looks like to the switcher, most-recently
//! used ordering and filtering. Platform independent.

use std::collections::HashMap;

use crate::config::Config;

/// Native window id: `CGWindowID` on macOS, `HWND` on Windows.
pub type WindowId = u64;

/// Rectangle in global coordinates (points on macOS, pixels on Windows;
/// top-left origin, y grows down).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn intersection_area(&self, o: &RectF) -> f64 {
        let w = self.right().min(o.right()) - self.x.max(o.x);
        let h = self.bottom().min(o.bottom()) - self.y.max(o.y);
        if w > 0.0 && h > 0.0 {
            w * h
        } else {
            0.0
        }
    }
    pub fn inset(&self, d: f64) -> RectF {
        RectF::new(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }
    pub fn offset(&self, dx: f64, dy: f64) -> RectF {
        RectF::new(self.x + dx, self.y + dy, self.w, self.h)
    }
}

/// One switchable window.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowInfo {
    pub id: WindowId,
    pub pid: i64,
    /// Display name of the owning app.
    pub app_name: String,
    /// Bundle id (macOS) or executable path (Windows).
    pub app_id: String,
    /// Path of the app bundle / executable, if known.
    pub app_path: String,
    pub title: String,
    /// Last known frame (global, top-left origin).
    pub frame: RectF,
    pub minimized: bool,
    /// The owning app is hidden (Cmd+H).
    pub app_hidden: bool,
    /// On a Space / virtual desktop that isn't currently visible.
    pub other_space: bool,
    pub fullscreen: bool,
    /// Owned by this process (PowerMac's own windows are never listed).
    pub own: bool,
}

impl WindowInfo {
    /// Width / height of the window (for thumbnail tiles), clamped to sane values.
    pub fn aspect(&self) -> f64 {
        if self.frame.w > 1.0 && self.frame.h > 1.0 {
            (self.frame.w / self.frame.h).clamp(0.5, 2.4)
        } else {
            1.6
        }
    }

    /// Index of the screen this window is mostly on (`None` if off-screen).
    pub fn screen_index(&self, screens: &[RectF]) -> Option<usize> {
        let mut best = None;
        let mut best_area = 0.0;
        for (i, s) in screens.iter().enumerate() {
            let a = s.intersection_area(&self.frame);
            if a > best_area {
                best_area = a;
                best = Some(i);
            }
        }
        best
    }

    /// Text shown under the thumbnail.
    pub fn display_title(&self) -> &str {
        if self.title.trim().is_empty() {
            &self.app_name
        } else {
            &self.title
        }
    }
}

/// Which windows a shortcut lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Every window of every app.
    AllWindows,
    /// Only the frontmost app's windows.
    CurrentApp,
}

/// Most-recently-used order of window ids (front = most recent).
#[derive(Debug, Clone, Default)]
pub struct Mru {
    order: Vec<WindowId>,
}

/// Cap so a long session can't grow the list without bound.
const MRU_CAP: usize = 512;

impl Mru {
    pub fn new() -> Self {
        Self::default()
    }

    /// `id` was just focused.
    pub fn touch(&mut self, id: WindowId) {
        if self.order.first() == Some(&id) {
            return;
        }
        self.order.retain(|&w| w != id);
        self.order.insert(0, id);
        self.order.truncate(MRU_CAP);
    }

    /// Seed with an initial (e.g. z-) order without disturbing ids already
    /// known: unknown ids are appended in the given order.
    pub fn seed(&mut self, ids: impl IntoIterator<Item = WindowId>) {
        for id in ids {
            if !self.order.contains(&id) && self.order.len() < MRU_CAP {
                self.order.push(id);
            }
        }
    }

    /// Forget windows that no longer exist.
    pub fn retain(&mut self, alive: impl Fn(WindowId) -> bool) {
        self.order.retain(|&w| alive(w));
    }

    pub fn ids(&self) -> &[WindowId] {
        &self.order
    }

    pub fn most_recent(&self) -> Option<WindowId> {
        self.order.first().copied()
    }

    /// Sort `windows` by recency. Windows never focused keep their relative
    /// input order (callers pass z-order) and go after the known ones.
    pub fn sort(&self, windows: &mut [WindowInfo]) {
        let rank: HashMap<WindowId, usize> = self.order.iter().enumerate().map(|(i, &w)| (w, i)).collect();
        // Stable sort keeps the input order among unknown windows.
        windows.sort_by_key(|w| rank.get(&w.id).copied().unwrap_or(usize::MAX));
    }
}

/// Context for [`filter`] that doesn't come from the config.
#[derive(Debug, Clone, Default)]
pub struct FilterContext {
    /// Frontmost app, for [`Mode::CurrentApp`].
    pub active_pid: Option<i64>,
    /// Screen frames (global), for `only_current_screen`.
    pub screens: Vec<RectF>,
    /// Index into `screens` of the screen with the pointer.
    pub current_screen: Option<usize>,
}

/// Apply the config's visibility rules and the mode to an (already ordered)
/// window list.
pub fn filter(windows: Vec<WindowInfo>, cfg: &Config, mode: Mode, ctx: &FilterContext) -> Vec<WindowInfo> {
    windows
        .into_iter()
        .filter(|w| {
            if w.own {
                return false;
            }
            if w.minimized && !cfg.show_minimized {
                return false;
            }
            if w.app_hidden && !cfg.show_hidden {
                return false;
            }
            if w.other_space && !cfg.show_other_spaces {
                return false;
            }
            if mode == Mode::CurrentApp && ctx.active_pid.is_some_and(|p| p != w.pid) {
                return false;
            }
            if cfg.only_current_screen {
                if let Some(cur) = ctx.current_screen {
                    // Minimized windows and windows of hidden apps keep the frame
                    // they had; use it to place them too.
                    if w.screen_index(&ctx.screens).is_some_and(|s| s != cur) {
                        return false;
                    }
                }
            }
            !cfg.is_excluded(&[&w.app_id, &w.app_name, &w.app_path])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(id: WindowId, pid: i64) -> WindowInfo {
        WindowInfo {
            id,
            pid,
            app_name: format!("App{pid}"),
            app_id: format!("com.example.app{pid}"),
            title: format!("Window {id}"),
            frame: RectF::new(100.0, 100.0, 800.0, 600.0),
            ..Default::default()
        }
    }

    fn ids(ws: &[WindowInfo]) -> Vec<WindowId> {
        ws.iter().map(|w| w.id).collect()
    }

    #[test]
    fn mru_touch_moves_to_front() {
        let mut m = Mru::new();
        m.seed([1, 2, 3, 4]);
        assert_eq!(m.ids(), &[1, 2, 3, 4]);
        m.touch(3);
        assert_eq!(m.ids(), &[3, 1, 2, 4]);
        m.touch(3);
        assert_eq!(m.ids(), &[3, 1, 2, 4]);
        m.touch(9);
        assert_eq!(m.ids(), &[9, 3, 1, 2, 4]);
        // Seeding doesn't reorder known ids.
        m.seed([4, 1, 7]);
        assert_eq!(m.ids(), &[9, 3, 1, 2, 4, 7]);
        m.retain(|w| w % 2 == 1);
        assert_eq!(m.ids(), &[9, 3, 1, 7]);
        assert_eq!(m.most_recent(), Some(9));
    }

    #[test]
    fn mru_sort_known_first_unknown_keep_order() {
        let mut m = Mru::new();
        m.touch(5);
        m.touch(2); // order: 2, 5
        let mut ws = vec![win(1, 1), win(5, 1), win(3, 2), win(2, 3), win(4, 2)];
        m.sort(&mut ws);
        assert_eq!(ids(&ws), vec![2, 5, 1, 3, 4]);
    }

    #[test]
    fn mru_is_capped() {
        let mut m = Mru::new();
        for i in 0..(MRU_CAP as u64 + 50) {
            m.touch(i);
        }
        assert_eq!(m.ids().len(), MRU_CAP);
        assert_eq!(m.most_recent(), Some(MRU_CAP as u64 + 49));
    }

    #[test]
    fn filter_rules() {
        let mut ws = vec![win(1, 1), win(2, 1), win(3, 2), win(4, 3), win(5, 4), win(6, 5)];
        ws[1].minimized = true;
        ws[2].app_hidden = true;
        ws[3].other_space = true;
        ws[4].own = true;
        ws[5].app_id = "com.apple.Safari".into();
        let ctx = FilterContext::default();
        let all = Config { excluded_apps: vec!["com.apple.safari".into()], ..Default::default() };
        assert_eq!(ids(&filter(ws.clone(), &all, Mode::AllWindows, &ctx)), vec![1, 2, 3, 4]);

        let strict = Config { show_minimized: false, show_hidden: false, show_other_spaces: false, ..Default::default() };
        assert_eq!(ids(&filter(ws.clone(), &strict, Mode::AllWindows, &ctx)), vec![1, 6]);

        let ctx_app = FilterContext { active_pid: Some(1), ..Default::default() };
        assert_eq!(ids(&filter(ws.clone(), &Config::default(), Mode::CurrentApp, &ctx_app)), vec![1, 2]);
        // CurrentApp without a known frontmost app lists everything.
        assert_eq!(filter(ws, &Config::default(), Mode::CurrentApp, &ctx).len(), 5);
    }

    #[test]
    fn filter_current_screen() {
        let screens = vec![RectF::new(0.0, 0.0, 1440.0, 900.0), RectF::new(1440.0, 0.0, 1920.0, 1080.0)];
        let mut a = win(1, 1);
        a.frame = RectF::new(1500.0, 50.0, 800.0, 600.0); // screen 1
        let mut b = win(2, 1);
        b.frame = RectF::new(1200.0, 50.0, 800.0, 600.0); // mostly screen 1 (560 vs 240)
        let c = win(3, 1); // screen 0
        let mut d = win(4, 1);
        d.frame = RectF::new(-5000.0, -5000.0, 10.0, 10.0); // off-screen: kept
        let cfg = Config { only_current_screen: true, ..Default::default() };
        let ctx = FilterContext { screens: screens.clone(), current_screen: Some(1), active_pid: None };
        assert_eq!(ids(&filter(vec![a.clone(), b.clone(), c.clone(), d.clone()], &cfg, Mode::AllWindows, &ctx)), vec![1, 2, 4]);
        let ctx0 = FilterContext { screens, current_screen: Some(0), active_pid: None };
        assert_eq!(ids(&filter(vec![a, b, c, d], &cfg, Mode::AllWindows, &ctx0)), vec![3, 4]);
    }

    #[test]
    fn display_title_falls_back_to_app() {
        let mut w = win(1, 1);
        w.title = "  ".into();
        assert_eq!(w.display_title(), "App1");
        assert!((w.aspect() - 800.0 / 600.0).abs() < 1e-9);
        w.frame = RectF::default();
        assert_eq!(w.aspect(), 1.6);
    }
}
