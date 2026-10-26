//! Platform-independent logic: input state machines (double Ctrl, shake,
//! hotkeys), animation timing, and the [`Scene`] that renderers draw.
//!
//! Coordinates are global screen coordinates with a top-left origin (the
//! CoreGraphics space on macOS, physical pixels on Windows). Sizes in the
//! scene are logical (points / DIPs); renderers multiply by the monitor scale.

use std::time::{Duration, Instant};

use crate::config::{Color, Config, CrosshairsConfig, HighlighterConfig, Mods, Orientation};

pub const VK_LCONTROL: u32 = 0xA2;
pub const VK_RCONTROL: u32 = 0xA3;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub fn dist(self, o: Point) -> f64 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
    }
}

/// Axis-aligned rectangle, `x0 <= x1`, `y0 <= y1` when non-empty.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl RectF {
    pub const fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0, y0, x1, y1 }
    }
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    pub fn intersect(&self, o: &RectF) -> RectF {
        RectF::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1))
    }
    pub fn inflate(&self, d: f64) -> RectF {
        RectF::new(self.x0 - d, self.y0 - d, self.x1 + d, self.y1 + d)
    }
    pub fn offset(&self, dx: f64, dy: f64) -> RectF {
        RectF::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x0 && p.x < self.x1 && p.y >= self.y0 && p.y < self.y1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

// ------------------------------------------------------------------ scene

#[derive(Debug, Clone, PartialEq)]
pub struct SpotlightShape {
    pub center: Point,
    pub radius: f64,
    /// Backdrop color; `a` already includes opacity and fade.
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CircleShape {
    pub center: Point,
    pub radius: f64,
    /// `a` already includes fade.
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CrosshairShape {
    pub center: Point,
    pub color: Color,
    pub border_color: Color,
    pub opacity: f64,
    pub center_radius: f64,
    pub thickness: f64,
    pub border: f64,
    pub orientation: Orientation,
    /// 0 = to the screen edges.
    pub fixed_length: f64,
}

/// One crosshair arm: `outer` is filled with the border color, then
/// `inner` with the line color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arm {
    pub outer: RectF,
    pub inner: RectF,
}

impl CrosshairShape {
    fn from_config(c: &CrosshairsConfig, center: Point) -> Self {
        Self {
            center,
            color: c.color,
            border_color: c.border_color,
            opacity: c.opacity.clamp(0.0, 1.0),
            center_radius: c.center_radius as f64,
            thickness: c.thickness.max(1) as f64,
            border: c.border_size as f64,
            orientation: c.orientation,
            fixed_length: c.fixed_length as f64,
        }
    }

    /// Arms clipped to `bounds` (a screen, same coordinate space as
    /// `center`), with logical sizes multiplied by `scale`.
    pub fn arms(&self, bounds: RectF, scale: f64) -> Vec<Arm> {
        let (cx, cy) = (self.center.x, self.center.y);
        let r = self.center_radius * scale;
        let half = self.thickness * scale / 2.0;
        let b = self.border * scale;
        let len = if self.fixed_length > 0.0 { self.fixed_length * scale } else { f64::INFINITY };
        let far = |l: f64| if l.is_finite() { l } else { 1.0e7 };
        let mut inner = Vec::with_capacity(4);
        if self.orientation != Orientation::Vertical {
            inner.push(RectF::new(cx - r - far(len), cy - half, cx - r, cy + half)); // left
            inner.push(RectF::new(cx + r, cy - half, cx + r + far(len), cy + half)); // right
        }
        if self.orientation != Orientation::Horizontal {
            inner.push(RectF::new(cx - half, cy - r - far(len), cx + half, cy - r)); // up
            inner.push(RectF::new(cx - half, cy + r, cx + half, cy + r + far(len))); // down
        }
        inner
            .into_iter()
            .filter_map(|i| {
                let outer = i.inflate(b).intersect(&bounds);
                if outer.is_empty() {
                    return None;
                }
                Some(Arm { outer, inner: i.intersect(&bounds) })
            })
            .collect()
    }
}

/// Everything to draw this frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    pub spotlight: Option<SpotlightShape>,
    pub circles: Vec<CircleShape>,
    pub crosshairs: Option<CrosshairShape>,
}

impl Scene {
    pub fn is_empty(&self) -> bool {
        self.spotlight.is_none() && self.circles.is_empty() && self.crosshairs.is_none()
    }
}

// ------------------------------------------------------------------ easing

/// Animation progress in `0..=1` for `elapsed` out of `duration` (a zero
/// duration completes immediately).
pub fn progress(elapsed: Duration, duration: Duration) -> f64 {
    if duration.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
}

pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Opacity factor of a click highlight released `since_release` ago:
/// 1 during `delay`, then linear down to 0 over `duration`. `None` = done.
pub fn fade_factor(since_release: Duration, delay: Duration, duration: Duration) -> Option<f64> {
    if since_release < delay {
        return Some(1.0);
    }
    let t = since_release - delay;
    if t >= duration {
        return None;
    }
    Some(1.0 - progress(t, duration))
}

// ------------------------------------------------------------------ double Ctrl

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DpState {
    Idle,
    Down1,
    Up1,
    Down2,
    Up2,
}

/// What a Ctrl event did to the double-press detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoublePressEvent {
    None,
    /// Second press detected: show the spotlight.
    Activate,
    /// Third press after activation: hide it.
    Deactivate,
}

/// PowerToys' "press Ctrl twice" state machine: the second press must come
/// between `min(100ms, dbl/5)` and the system double-click time after the
/// first, with the cursor not moved.
#[derive(Debug, Clone)]
pub struct DoublePress {
    state: DpState,
    last_time: Option<Instant>,
    last_pos: Point,
}

impl Default for DoublePress {
    fn default() -> Self {
        Self { state: DpState::Idle, last_time: None, last_pos: Point::default() }
    }
}

impl DoublePress {
    pub fn reset(&mut self) {
        self.state = DpState::Idle;
    }

    pub fn key(&mut self, down: bool, now: Instant, cursor: Point, double_click: Duration) -> DoublePressEvent {
        match self.state {
            DpState::Idle => {
                if down {
                    self.state = DpState::Down1;
                    self.last_time = Some(now);
                    self.last_pos = cursor;
                }
            }
            DpState::Down1 => {
                if !down {
                    self.state = DpState::Up1;
                }
            }
            DpState::Up1 => {
                if down {
                    let min = Duration::from_millis(100).min(double_click / 5);
                    let dt = self.last_time.map(|t| now.saturating_duration_since(t)).unwrap_or(Duration::MAX);
                    let ok = dt >= min && dt <= double_click && cursor.dist(self.last_pos) <= 2.0;
                    self.last_time = Some(now);
                    self.last_pos = cursor;
                    if ok {
                        self.state = DpState::Down2;
                        return DoublePressEvent::Activate;
                    }
                    self.state = DpState::Down1;
                }
            }
            DpState::Down2 => {
                if !down {
                    self.state = DpState::Up2;
                }
            }
            DpState::Up2 => {
                if down {
                    self.state = DpState::Idle;
                    return DoublePressEvent::Deactivate;
                }
            }
        }
        DoublePressEvent::None
    }
}

// ------------------------------------------------------------------ shake

#[derive(Debug, Clone, Copy)]
struct Movement {
    dx: f64,
    dy: f64,
    at: Instant,
}

/// PowerToys' shake detector: within the last `interval`, has the pointer
/// travelled much further (`factor`) than the diagonal of the box it moved in?
#[derive(Debug, Clone, Default)]
pub struct ShakeDetector {
    history: Vec<Movement>,
}

impl ShakeDetector {
    pub fn clear(&mut self) {
        self.history.clear();
    }

    /// Feed a relative movement. Returns true when a shake is detected.
    pub fn movement(&mut self, dx: f64, dy: f64, now: Instant, min_distance: f64, interval: Duration, factor: f64) -> bool {
        if dx == 0.0 && dy == 0.0 {
            return false;
        }
        let sign = |v: f64| if v > 0.0 { 1 } else if v < 0.0 { -1 } else { 0 };
        if let Some(last) = self.history.last_mut() {
            if sign(last.dx) == sign(dx) && sign(last.dy) == sign(dy) {
                last.dx += dx;
                last.dy += dy;
                return false;
            }
        } else {
            self.history.push(Movement { dx, dy, at: now });
            return false;
        }
        self.history.push(Movement { dx, dy, at: now });
        // Direction changed: check for a shake.
        self.history.retain(|m| now.saturating_duration_since(m.at) <= interval);
        let (mut x, mut y, mut min_x, mut max_x, mut min_y, mut max_y, mut dist) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for m in &self.history {
            x += m.dx;
            y += m.dy;
            dist += (m.dx * m.dx + m.dy * m.dy).sqrt();
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        if dist < min_distance {
            return false;
        }
        let diag = ((max_x - min_x).powi(2) + (max_y - min_y).powi(2)).sqrt();
        if diag > 0.0 && dist / diag > factor {
            self.history.clear();
            return true;
        }
        false
    }
}

// ------------------------------------------------------------------ modifiers

/// Tracks held modifiers from left/right-specific VK events (Windows LL hooks).
#[derive(Debug, Clone, Copy, Default)]
pub struct ModTracker {
    held: u16,
}

impl ModTracker {
    fn bit(vk: u32) -> Option<u16> {
        Some(match vk {
            0xA0 => 1,      // LSHIFT
            0xA1 => 2,      // RSHIFT
            0xA2 => 4,      // LCONTROL
            0xA3 => 8,      // RCONTROL
            0xA4 => 16,     // LMENU
            0xA5 => 32,     // RMENU
            0x5B => 64,     // LWIN
            0x5C => 128,    // RWIN
            0x10 => 1,      // generic SHIFT
            0x11 => 4,      // generic CONTROL
            0x12 => 16,     // generic MENU
            _ => return None,
        })
    }

    pub fn update(&mut self, vk: u32, down: bool) {
        if let Some(b) = Self::bit(vk) {
            if down {
                self.held |= b;
            } else {
                self.held &= !b;
            }
        }
    }

    pub fn mods(&self) -> Mods {
        Mods {
            shift: self.held & 3 != 0,
            ctrl: self.held & 12 != 0,
            alt: self.held & 48 != 0,
            win: self.held & 192 != 0,
        }
    }
}

// ------------------------------------------------------------------ engine

#[derive(Debug, Clone)]
struct Spotlight {
    opened: Instant,
    /// Cursor position last frame; None until the first frame.
    last_pos: Option<Point>,
    /// When the cursor last moved (None = still waiting for the first move).
    moved_at: Option<Instant>,
    /// (when, alpha factor, radius) at the moment closing began.
    closing: Option<(Instant, f64, f64)>,
}

#[derive(Debug, Clone)]
struct Ripple {
    button: Button,
    pos: Point,
    released: Option<Instant>,
}

/// All feature state. Shared behind a mutex by the platform runtimes.
#[derive(Debug)]
pub struct Engine {
    cfg: Config,
    double_click: Duration,
    dp: DoublePress,
    shake: ShakeDetector,
    spotlight: Option<Spotlight>,
    highlighter_on: bool,
    crosshairs_on: bool,
    ripples: Vec<Ripple>,
    swallow_up: Option<u32>,
    cursor: Point,
}

impl Engine {
    /// `double_click` is the system double-click interval (used by the
    /// double-Ctrl detector).
    pub fn new(cfg: Config, double_click: Duration) -> Self {
        let mut e = Self {
            highlighter_on: cfg.highlighter.enabled && cfg.highlighter.auto_activate,
            crosshairs_on: cfg.crosshairs.enabled && cfg.crosshairs.auto_activate,
            cfg,
            double_click: if double_click.is_zero() { Duration::from_millis(500) } else { double_click },
            dp: DoublePress::default(),
            shake: ShakeDetector::default(),
            spotlight: None,
            ripples: Vec::new(),
            swallow_up: None,
            cursor: Point::default(),
        };
        e.apply_enabled();
        e
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn set_config(&mut self, cfg: Config) {
        self.cfg = cfg;
        self.apply_enabled();
    }

    fn apply_enabled(&mut self) {
        if !self.cfg.find_my_mouse.enabled {
            self.spotlight = None;
            self.dp.reset();
        }
        if !self.cfg.find_my_mouse.activation_method.uses_shake() {
            self.shake.clear();
        }
        if !self.cfg.highlighter.enabled {
            self.highlighter_on = false;
            self.ripples.clear();
        }
        if !self.cfg.crosshairs.enabled {
            self.crosshairs_on = false;
        }
    }

    pub fn highlighter_on(&self) -> bool {
        self.highlighter_on
    }
    pub fn crosshairs_on(&self) -> bool {
        self.crosshairs_on
    }
    pub fn spotlight_active(&self) -> bool {
        self.spotlight.as_ref().is_some_and(|s| s.closing.is_none())
    }

    /// Whether the renderer must keep producing frames.
    pub fn needs_frames(&self) -> bool {
        self.spotlight.is_some()
            || !self.ripples.is_empty()
            || self.crosshairs_on
            || (self.highlighter_on && self.cfg.highlighter.highlight_cursor)
    }

    pub fn toggle_highlighter(&mut self) -> bool {
        if self.cfg.highlighter.enabled {
            self.highlighter_on = !self.highlighter_on;
            if !self.highlighter_on {
                self.ripples.clear();
            }
        }
        self.highlighter_on
    }

    pub fn toggle_crosshairs(&mut self) -> bool {
        if self.cfg.crosshairs.enabled {
            self.crosshairs_on = !self.crosshairs_on;
        }
        self.crosshairs_on
    }

    /// Show the spotlight (restarts it if it is fading out). No-op when Find
    /// My Mouse is disabled.
    pub fn find_mouse(&mut self, now: Instant) {
        if !self.cfg.find_my_mouse.enabled {
            return;
        }
        if self.spotlight_active() {
            return;
        }
        self.spotlight = Some(Spotlight { opened: now, last_pos: None, moved_at: None, closing: None });
        self.shake.clear();
    }

    /// Begin fading the spotlight out.
    pub fn dismiss_spotlight(&mut self, now: Instant) {
        self.dp.reset();
        let Some(s) = &mut self.spotlight else { return };
        if s.closing.is_some() {
            return;
        }
        let f = &self.cfg.find_my_mouse;
        let dur = Duration::from_millis(f.animation_duration_ms as u64);
        let e = ease_out_cubic(progress(now.saturating_duration_since(s.opened), dur));
        let r = spot_radius(f.spotlight_radius as f64, f.initial_zoom as f64, e);
        s.closing = Some((now, e, r));
    }

    /// A key event (`vk` is a Windows VK; Ctrl must be `VK_LCONTROL` /
    /// `VK_RCONTROL`). `mods` = modifiers held, excluding this key.
    /// Returns true if the event should be swallowed (it was a hotkey).
    pub fn on_key(&mut self, vk: u32, down: bool, mods: Mods, now: Instant) -> bool {
        if !down {
            if self.swallow_up == Some(vk) {
                self.swallow_up = None;
                return true;
            }
            if let Some(right) = self.ctrl_target() {
                if vk == if right { VK_RCONTROL } else { VK_LCONTROL } {
                    let _ = self.dp.key(false, now, self.cursor, self.double_click);
                }
            }
            return false;
        }

        // Find My Mouse: double Ctrl, else any key dismisses.
        let mut handled_by_dp = false;
        if let Some(right) = self.ctrl_target() {
            if vk == if right { VK_RCONTROL } else { VK_LCONTROL } {
                handled_by_dp = true;
                match self.dp.key(true, now, self.cursor, self.double_click) {
                    DoublePressEvent::Activate => self.find_mouse(now),
                    DoublePressEvent::Deactivate => self.dismiss_spotlight(now),
                    DoublePressEvent::None => {}
                }
            }
        }
        if !handled_by_dp {
            self.dp.reset();
            self.dismiss_spotlight(now);
        }

        // Hotkeys.
        let fm = &self.cfg.find_my_mouse;
        if fm.enabled && fm.activation_method == crate::config::ActivationMethod::Shortcut && fm.hotkey.matches(vk, mods) {
            if self.spotlight_active() {
                self.dismiss_spotlight(now);
            } else {
                self.find_mouse(now);
            }
            self.swallow_up = Some(vk);
            return true;
        }
        if self.cfg.highlighter.enabled && self.cfg.highlighter.hotkey.matches(vk, mods) {
            self.toggle_highlighter();
            self.swallow_up = Some(vk);
            return true;
        }
        if self.cfg.crosshairs.enabled && self.cfg.crosshairs.hotkey.matches(vk, mods) {
            self.toggle_crosshairs();
            self.swallow_up = Some(vk);
            return true;
        }
        false
    }

    /// Whether key-ups / auto-repeats of `vk` are being swallowed (a hotkey
    /// key is still held).
    pub fn swallowing(&self, vk: u32) -> bool {
        self.swallow_up == Some(vk)
    }

    fn ctrl_target(&self) -> Option<bool> {
        let f = &self.cfg.find_my_mouse;
        if !f.enabled {
            return None;
        }
        f.activation_method.uses_double_ctrl()
    }

    /// Pointer moved to `pos`. `delta` is the raw relative motion if the
    /// platform provides it (otherwise it is derived from positions).
    pub fn on_mouse_move(&mut self, pos: Point, delta: Option<(f64, f64)>, now: Instant) {
        let (dx, dy) = delta.unwrap_or((pos.x - self.cursor.x, pos.y - self.cursor.y));
        self.cursor = pos;
        for r in self.ripples.iter_mut().filter(|r| r.released.is_none()) {
            r.pos = pos;
        }
        let f = &self.cfg.find_my_mouse;
        if f.enabled && f.activation_method.uses_shake() && !self.spotlight_active() {
            let hit = self.shake.movement(
                dx,
                dy,
                now,
                f.shake_minimum_distance as f64,
                Duration::from_millis(f.shake_interval_ms as u64),
                f.shake_factor as f64 / 100.0,
            );
            if hit {
                self.find_mouse(now);
            }
        }
    }

    pub fn on_button(&mut self, button: Button, down: bool, pos: Point, now: Instant) {
        self.cursor = pos;
        if down {
            self.dismiss_spotlight(now);
            if self.highlighter_on {
                // A new press of the same button replaces a held one.
                for r in self.ripples.iter_mut().filter(|r| r.button == button && r.released.is_none()) {
                    r.released = Some(now);
                }
                self.ripples.push(Ripple { button, pos, released: None });
            }
        } else {
            for r in self.ripples.iter_mut().filter(|r| r.button == button && r.released.is_none()) {
                r.released = Some(now);
                r.pos = pos;
            }
        }
    }

    /// Advance animations and describe what to draw. `cursor` is the polled
    /// pointer position.
    pub fn frame(&mut self, now: Instant, cursor: Point) -> Scene {
        self.cursor = cursor;
        let mut scene = Scene::default();

        // Find My Mouse.
        let f = self.cfg.find_my_mouse.clone();
        let anim = Duration::from_millis(f.animation_duration_ms as u64);
        let mut close_now = false;
        let mut done = false;
        if let Some(s) = &mut self.spotlight {
            if s.closing.is_none() {
                match s.last_pos {
                    None => s.last_pos = Some(cursor),
                    Some(p) if p.dist(cursor) > 0.5 => {
                        s.last_pos = Some(cursor);
                        s.moved_at = Some(now);
                    }
                    _ => {}
                }
                if s.moved_at.is_some_and(|t| now.saturating_duration_since(t) >= Duration::from_millis(f.idle_timeout_ms as u64)) {
                    close_now = true;
                }
            }
            let (k, radius) = match s.closing {
                Some((t, k0, r)) => {
                    let q = progress(now.saturating_duration_since(t), anim);
                    if q >= 1.0 {
                        done = true;
                    }
                    (k0 * (1.0 - q), r)
                }
                None => {
                    let e = ease_out_cubic(progress(now.saturating_duration_since(s.opened), anim));
                    (e, spot_radius(f.spotlight_radius as f64, f.initial_zoom as f64, e))
                }
            };
            if !done {
                let a = (f.overlay_opacity.clamp(0.0, 1.0) * k * 255.0).round() as u8;
                scene.spotlight = Some(SpotlightShape { center: cursor, radius, color: Color { a, ..f.backdrop_color } });
            }
        }
        if done {
            self.spotlight = None;
            self.dp.reset();
        } else if close_now {
            self.dismiss_spotlight(now);
        }

        // Highlighter.
        let h: &HighlighterConfig = &self.cfg.highlighter;
        let delay = Duration::from_millis(h.fade_delay_ms as u64);
        let dur = Duration::from_millis(h.fade_duration_ms as u64);
        let radius = h.radius as f64;
        let mut held = false;
        self.ripples.retain_mut(|r| {
            let k = match r.released {
                None => {
                    r.pos = cursor;
                    held = true;
                    1.0
                }
                Some(t) => match fade_factor(now.saturating_duration_since(t), delay, dur) {
                    Some(k) => k,
                    None => return false,
                },
            };
            let base = if r.button == Button::Left { h.left_button_color } else { h.right_button_color };
            scene.circles.push(CircleShape { center: r.pos, radius, color: base.scaled_alpha(k) });
            true
        });
        if self.highlighter_on && h.highlight_cursor && !held {
            scene.circles.insert(0, CircleShape { center: cursor, radius, color: h.always_color });
        }

        // Crosshairs.
        if self.crosshairs_on {
            scene.crosshairs = Some(CrosshairShape::from_config(&self.cfg.crosshairs, cursor));
        }
        scene
    }
}

/// Spotlight radius at eased progress `e`: starts `zoom` times larger.
pub fn spot_radius(radius: f64, zoom: f64, e: f64) -> f64 {
    let zoom = zoom.max(1.0);
    radius * (zoom - (zoom - 1.0) * e.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ActivationMethod, Hotkey};

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    const DBL: Duration = Duration::from_millis(500);

    #[test]
    fn double_press_timing() {
        let t0 = Instant::now();
        let p = Point::new(10.0, 10.0);
        let mut d = DoublePress::default();
        assert_eq!(d.key(true, t0, p, DBL), DoublePressEvent::None);
        assert_eq!(d.key(false, ms(t0, 50), p, DBL), DoublePressEvent::None);
        assert_eq!(d.key(true, ms(t0, 200), p, DBL), DoublePressEvent::Activate);
        assert_eq!(d.key(false, ms(t0, 250), p, DBL), DoublePressEvent::None);
        assert_eq!(d.key(true, ms(t0, 900), p, DBL), DoublePressEvent::Deactivate);

        // Too slow: second press becomes a new first press.
        let mut d = DoublePress::default();
        d.key(true, t0, p, DBL);
        d.key(false, ms(t0, 100), p, DBL);
        assert_eq!(d.key(true, ms(t0, 700), p, DBL), DoublePressEvent::None);
        d.key(false, ms(t0, 750), p, DBL);
        assert_eq!(d.key(true, ms(t0, 1000), p, DBL), DoublePressEvent::Activate);

        // Too fast (< 100ms) is rejected (bounce).
        let mut d = DoublePress::default();
        d.key(true, t0, p, DBL);
        d.key(false, ms(t0, 20), p, DBL);
        assert_eq!(d.key(true, ms(t0, 60), p, DBL), DoublePressEvent::None);

        // Cursor moved between presses.
        let mut d = DoublePress::default();
        d.key(true, t0, p, DBL);
        d.key(false, ms(t0, 50), p, DBL);
        assert_eq!(d.key(true, ms(t0, 200), Point::new(50.0, 10.0), DBL), DoublePressEvent::None);

        // Holding (key repeat) doesn't count as a second press.
        let mut d = DoublePress::default();
        d.key(true, t0, p, DBL);
        assert_eq!(d.key(true, ms(t0, 200), p, DBL), DoublePressEvent::None);
    }

    #[test]
    fn shake_detection() {
        let t0 = Instant::now();
        let mut s = ShakeDetector::default();
        let iv = Duration::from_millis(1000);
        // Back and forth 150px, 10 times in 500ms: travelled 1500, box ~150.
        let mut hit = false;
        for i in 0..10u64 {
            let dx = if i % 2 == 0 { 150.0 } else { -150.0 };
            hit |= s.movement(dx, 0.0, ms(t0, i * 50), 1000.0, iv, 4.0);
        }
        assert!(hit);

        // A long straight line is not a shake.
        let mut s = ShakeDetector::default();
        let mut hit = false;
        for i in 0..50u64 {
            hit |= s.movement(40.0, if i % 2 == 0 { 1.0 } else { -1.0 }, ms(t0, i * 10), 1000.0, iv, 4.0);
        }
        assert!(!hit);

        // Slow wiggles spread over too long don't accumulate.
        let mut s = ShakeDetector::default();
        let mut hit = false;
        for i in 0..10u64 {
            let dx = if i % 2 == 0 { 150.0 } else { -150.0 };
            hit |= s.movement(dx, 0.0, ms(t0, i * 400), 1000.0, iv, 4.0);
        }
        assert!(!hit);
    }

    #[test]
    fn fade_and_easing_math() {
        let d = Duration::from_millis;
        assert_eq!(fade_factor(d(100), d(400), d(400)), Some(1.0));
        assert_eq!(fade_factor(d(600), d(400), d(400)), Some(0.5));
        assert_eq!(fade_factor(d(800), d(400), d(400)), None);
        assert_eq!(fade_factor(d(0), d(0), d(0)), None);
        assert_eq!(progress(d(250), d(500)), 0.5);
        assert_eq!(progress(d(900), d(500)), 1.0);
        assert_eq!(progress(d(1), Duration::ZERO), 1.0);
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!((ease_out_cubic(0.5) - 0.875).abs() < 1e-9);
        assert_eq!(spot_radius(100.0, 9.0, 0.0), 900.0);
        assert_eq!(spot_radius(100.0, 9.0, 1.0), 100.0);
        assert_eq!(spot_radius(100.0, 0.0, 0.0), 100.0);
    }

    #[test]
    fn mod_tracker() {
        let mut m = ModTracker::default();
        m.update(0x5B, true);
        m.update(0xA1, true);
        assert_eq!(m.mods(), Mods::new(true, false, false, true));
        m.update(0xA1, false);
        m.update(0xA2, true);
        assert_eq!(m.mods(), Mods::new(true, true, false, false));
        m.update(0x5B, false);
        m.update(0xA2, false);
        assert_eq!(m.mods(), Mods::default());
    }

    fn engine() -> Engine {
        Engine::new(Config::default(), DBL)
    }

    #[test]
    fn spotlight_lifecycle() {
        let t0 = Instant::now();
        let mut e = engine();
        let c = Point::new(500.0, 400.0);
        e.on_mouse_move(c, None, t0);
        // double left ctrl
        assert!(!e.on_key(VK_LCONTROL, true, Mods::default(), t0));
        e.on_key(VK_LCONTROL, false, Mods::default(), ms(t0, 60));
        e.on_key(VK_LCONTROL, true, Mods::new(false, false, false, false), ms(t0, 200));
        assert!(e.spotlight_active());
        e.on_key(VK_LCONTROL, false, Mods::default(), ms(t0, 260));
        assert!(e.spotlight_active(), "releasing the 2nd press keeps it");

        let s = e.frame(ms(t0, 200), c);
        let sp = s.spotlight.unwrap();
        assert_eq!(sp.radius, 900.0); // starts zoomed
        assert_eq!(sp.color.a, 0); // starts transparent
        let s = e.frame(ms(t0, 700), c);
        let sp = s.spotlight.unwrap();
        assert_eq!(sp.radius, 100.0);
        assert_eq!(sp.color.a, 128);
        // No movement yet: stays well past the idle timeout.
        assert!(e.frame(ms(t0, 5000), c).spotlight.is_some());
        assert!(e.spotlight_active());
        // Move, then stay still for 1s -> fades out over 500ms.
        let c2 = Point::new(520.0, 400.0);
        e.frame(ms(t0, 5100), c2);
        assert!(e.frame(ms(t0, 6000), c2).spotlight.is_some());
        assert!(e.spotlight_active());
        let s = e.frame(ms(t0, 6150), c2);
        assert!(!e.spotlight_active());
        assert_eq!(s.spotlight.unwrap().center, c2);
        let s = e.frame(ms(t0, 6400), c2).spotlight.unwrap();
        assert!(s.color.a > 0 && s.color.a < 128);
        assert!(e.frame(ms(t0, 6700), c2).spotlight.is_none());
        assert!(!e.needs_frames());
    }

    #[test]
    fn spotlight_dismissed_by_click_and_key() {
        let t0 = Instant::now();
        let mut e = engine();
        e.find_mouse(t0);
        assert!(e.spotlight_active());
        e.on_button(Button::Left, true, Point::default(), ms(t0, 10));
        assert!(!e.spotlight_active());
        e.frame(ms(t0, 1000), Point::default());
        assert!(e.spotlight.is_none());

        e.find_mouse(ms(t0, 2000));
        e.on_key(0x41, true, Mods::default(), ms(t0, 2100));
        assert!(!e.spotlight_active());

        // third ctrl press after activation hides it
        let mut e = engine();
        let t = ms(t0, 3000);
        e.on_key(VK_LCONTROL, true, Mods::default(), t);
        e.on_key(VK_LCONTROL, false, Mods::default(), ms(t0, 3050));
        e.on_key(VK_LCONTROL, true, Mods::default(), ms(t0, 3200));
        e.on_key(VK_LCONTROL, false, Mods::default(), ms(t0, 3250));
        assert!(e.spotlight_active());
        e.on_key(VK_LCONTROL, true, Mods::default(), ms(t0, 3600));
        assert!(!e.spotlight_active());

        // right ctrl doesn't trigger the left-ctrl method
        let mut e = engine();
        e.on_key(VK_RCONTROL, true, Mods::default(), t0);
        e.on_key(VK_RCONTROL, false, Mods::default(), ms(t0, 50));
        e.on_key(VK_RCONTROL, true, Mods::default(), ms(t0, 200));
        assert!(!e.spotlight_active());

        // disabled module
        let mut cfg = Config::default();
        cfg.find_my_mouse.enabled = false;
        let mut e = Engine::new(cfg, DBL);
        e.find_mouse(t0);
        assert!(!e.spotlight_active());
    }

    #[test]
    fn shake_activation_via_engine() {
        let t0 = Instant::now();
        let mut cfg = Config::default();
        cfg.find_my_mouse.activation_method = ActivationMethod::ShakeMouse;
        let mut e = Engine::new(cfg, DBL);
        for i in 0..12u64 {
            let dx = if i % 2 == 0 { 200.0 } else { -200.0 };
            e.on_mouse_move(Point::new(500.0 + if i % 2 == 0 { 200.0 } else { 0.0 }, 300.0), Some((dx, 0.0)), ms(t0, i * 40));
        }
        assert!(e.spotlight_active());
        // double ctrl is not active with the shake method
        let mut e = Engine::new(Config { find_my_mouse: crate::config::FindMyMouseConfig { activation_method: ActivationMethod::ShakeMouse, ..Default::default() }, ..Default::default() }, DBL);
        e.on_key(VK_LCONTROL, true, Mods::default(), t0);
        e.on_key(VK_LCONTROL, false, Mods::default(), ms(t0, 50));
        e.on_key(VK_LCONTROL, true, Mods::default(), ms(t0, 200));
        assert!(!e.spotlight_active());
    }

    #[test]
    fn hotkeys_toggle_and_swallow() {
        let t0 = Instant::now();
        let mut e = engine();
        let cmd_shift = Mods::new(true, false, false, true);
        assert!(e.on_key(0x48, true, cmd_shift, t0));
        assert!(e.highlighter_on());
        assert!(e.on_key(0x48, false, cmd_shift, t0), "matching key-up is swallowed too");
        assert!(!e.on_key(0x48, false, cmd_shift, t0));
        assert!(!e.on_key(0x48, true, Mods::new(false, false, false, true), t0));
        assert!(e.on_key(0x50, true, Mods::new(true, false, true, false), t0));
        assert!(e.crosshairs_on());
        assert!(e.needs_frames());
        assert!(e.on_key(0x50, true, Mods::new(true, false, true, false), t0));
        assert!(!e.crosshairs_on());

        // Disabled module: hotkey passes through, toggle is a no-op.
        let mut cfg = Config::default();
        cfg.crosshairs.enabled = false;
        cfg.highlighter.hotkey = Hotkey::none();
        let mut e = Engine::new(cfg, DBL);
        assert!(!e.on_key(0x50, true, Mods::new(true, false, true, false), t0));
        assert!(!e.toggle_crosshairs());
        assert!(!e.on_key(0x48, true, cmd_shift, t0));
        assert!(e.toggle_highlighter());
    }

    #[test]
    fn highlighter_ripples() {
        let t0 = Instant::now();
        let mut e = engine();
        let p = Point::new(100.0, 100.0);
        e.on_button(Button::Left, true, p, t0);
        assert!(e.frame(t0, p).circles.is_empty(), "off by default");
        e.toggle_highlighter();
        e.on_button(Button::Left, true, p, t0);
        e.on_button(Button::Right, true, p, t0);
        let s = e.frame(ms(t0, 1000), Point::new(120.0, 100.0));
        assert_eq!(s.circles.len(), 2);
        assert_eq!(s.circles[0].color, HighlighterConfig::default().left_button_color);
        assert_eq!(s.circles[1].color, HighlighterConfig::default().right_button_color);
        assert_eq!(s.circles[0].center, Point::new(120.0, 100.0), "held circle follows the cursor");
        e.on_button(Button::Left, false, Point::new(120.0, 100.0), ms(t0, 1000));
        let s = e.frame(ms(t0, 1600), Point::new(300.0, 300.0));
        assert_eq!(s.circles[0].center, Point::new(120.0, 100.0));
        assert_eq!(s.circles[0].color.a, 83); // half of 166
        let s = e.frame(ms(t0, 1900), Point::new(300.0, 300.0));
        assert_eq!(s.circles.len(), 1); // left done, right still held
        e.on_button(Button::Right, false, p, ms(t0, 1900));
        assert!(e.frame(ms(t0, 2800), p).circles.is_empty());
        assert!(!e.needs_frames());

        let mut cfg = Config::default();
        cfg.highlighter.highlight_cursor = true;
        cfg.highlighter.auto_activate = true;
        let mut e = Engine::new(cfg, DBL);
        assert!(e.highlighter_on());
        assert!(e.needs_frames());
        let s = e.frame(t0, p);
        assert_eq!(s.circles.len(), 1);
        assert_eq!(s.circles[0].center, p);
    }

    #[test]
    fn crosshair_geometry() {
        let c = CrosshairShape::from_config(&CrosshairsConfig::default(), Point::new(100.0, 50.0));
        let screen = RectF::new(0.0, 0.0, 200.0, 100.0);
        let arms = c.arms(screen, 1.0);
        assert_eq!(arms.len(), 4);
        // left arm: from screen edge to center - radius(20); thickness 5, border 1
        assert_eq!(arms[0].inner, RectF::new(0.0, 47.5, 80.0, 52.5));
        assert_eq!(arms[0].outer, RectF::new(0.0, 46.5, 81.0, 53.5));
        assert_eq!(arms[3].inner, RectF::new(97.5, 70.0, 102.5, 100.0));
        // scale 2
        let arms2 = c.arms(screen, 2.0);
        assert_eq!(arms2[1].inner, RectF::new(140.0, 45.0, 200.0, 55.0));
        // other screen below: only the vertical down arm reaches it
        let below = RectF::new(0.0, 100.0, 200.0, 300.0);
        let a = c.arms(below, 1.0);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].inner, RectF::new(97.5, 100.0, 102.5, 300.0));
        // fixed length + orientation
        let mut cfg = CrosshairsConfig { fixed_length: 10, orientation: Orientation::Horizontal, ..Default::default() };
        cfg.border_size = 0;
        let c = CrosshairShape::from_config(&cfg, Point::new(100.0, 50.0));
        let a = c.arms(screen, 1.0);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].inner, RectF::new(70.0, 47.5, 80.0, 52.5));
        assert_eq!(a[0].outer, a[0].inner);
    }
}
