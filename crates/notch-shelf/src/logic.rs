//! When to expand and collapse: a small, platform-independent state machine
//! fed once per frame with the pointer / drag situation.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Collapsed,
    Expanded,
    /// Briefly widened notch showing a status (e.g. charging).
    Peek,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Inputs {
    /// Seconds on any monotonic clock.
    pub now: f64,
    /// Pointer over the collapsed notch.
    pub in_hover_zone: bool,
    /// Pointer over the expanded panel (with hysteresis).
    pub in_keep_zone: bool,
    /// Pointer near the top center (drag activation zone).
    pub in_drag_zone: bool,
    /// Left mouse button held.
    pub button_down: bool,
    /// A drag session from another app / view is in progress.
    pub external_drag: bool,
    /// The platform reports a drag hovering the panel as a drop target.
    pub drag_over_panel: bool,
    /// Mouse went down over the collapsed notch this frame.
    pub clicked_notch: bool,
    /// Mouse went down outside the expanded panel this frame.
    pub mouse_down_outside: bool,
    /// Something needs the panel to stay open (menu, share sheet, our own
    /// drag-out session, a request from the API).
    pub pinned: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub expand_on_hover: bool,
    pub hover_delay: f64,
    pub collapse_delay: f64,
    pub expand_on_drag: bool,
    pub expand_on_click: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { expand_on_hover: true, hover_delay: 0.15, collapse_delay: 0.35, expand_on_drag: true, expand_on_click: true }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExpandLogic {
    mode: Option<Mode>,
    hover_since: Option<f64>,
    leave_since: Option<f64>,
    peek_until: Option<f64>,
    /// After a dismissal with the pointer still on the notch, hover must
    /// leave and come back before it can expand again.
    suppress_hover: bool,
}

impl ExpandLogic {
    pub fn mode(&self) -> Mode {
        self.mode.unwrap_or(Mode::Collapsed)
    }

    /// Show a status peek for `secs` (only while collapsed).
    pub fn peek(&mut self, now: f64, secs: f64) {
        if self.mode() != Mode::Expanded {
            self.mode = Some(Mode::Peek);
            self.peek_until = Some(now + secs);
        }
    }

    pub fn expand(&mut self) {
        self.mode = Some(Mode::Expanded);
        self.leave_since = None;
        self.hover_since = None;
        self.peek_until = None;
    }

    /// Collapse now (Escape, API, after "drag all"); `pointer_on_notch`
    /// suppresses an immediate hover re-expansion.
    pub fn collapse(&mut self, pointer_on_notch: bool) {
        self.mode = Some(Mode::Collapsed);
        self.leave_since = None;
        self.hover_since = None;
        self.suppress_hover = pointer_on_notch;
    }

    pub fn update(&mut self, s: &Settings, i: &Inputs) -> Mode {
        if !i.in_hover_zone {
            self.suppress_hover = false;
        }
        match self.mode() {
            Mode::Collapsed | Mode::Peek => {
                if self.mode() == Mode::Peek && self.peek_until.is_some_and(|t| i.now >= t) {
                    self.mode = Some(Mode::Collapsed);
                    self.peek_until = None;
                }
                let by_click = s.expand_on_click && i.clicked_notch;
                let by_drag = s.expand_on_drag && i.external_drag && (i.in_drag_zone || i.drag_over_panel);
                let hovering = s.expand_on_hover && i.in_hover_zone && !i.button_down && !self.suppress_hover;
                let by_hover = if hovering {
                    let since = *self.hover_since.get_or_insert(i.now);
                    i.now - since >= s.hover_delay
                } else {
                    self.hover_since = None;
                    false
                };
                if by_click || by_drag || by_hover {
                    self.expand();
                }
            }
            Mode::Expanded => {
                if i.mouse_down_outside && !i.external_drag && !i.pinned {
                    self.collapse(i.in_hover_zone);
                    return self.mode();
                }
                let keep = i.pinned || i.in_keep_zone || i.drag_over_panel || (i.external_drag && i.in_drag_zone);
                if keep {
                    self.leave_since = None;
                } else {
                    let since = *self.leave_since.get_or_insert(i.now);
                    if i.now - since >= s.collapse_delay {
                        self.collapse(false);
                    }
                }
            }
        }
        self.mode()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(now: f64) -> Inputs {
        Inputs { now, ..Default::default() }
    }

    #[test]
    fn hover_expands_after_delay_and_collapses_after_leaving() {
        let s = Settings::default();
        let mut l = ExpandLogic::default();
        assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, ..at(0.0) }), Mode::Collapsed);
        assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, ..at(0.1) }), Mode::Collapsed);
        assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, in_keep_zone: true, ..at(0.16) }), Mode::Expanded);
        assert_eq!(l.update(&s, &Inputs { in_keep_zone: true, ..at(1.0) }), Mode::Expanded);
        assert_eq!(l.update(&s, &at(1.1)), Mode::Expanded);
        assert_eq!(l.update(&s, &at(1.3)), Mode::Expanded);
        // Coming back within the delay cancels the collapse.
        assert_eq!(l.update(&s, &Inputs { in_keep_zone: true, ..at(1.35) }), Mode::Expanded);
        assert_eq!(l.update(&s, &at(1.4)), Mode::Expanded);
        assert_eq!(l.update(&s, &at(1.76)), Mode::Collapsed);
    }

    #[test]
    fn brief_hover_does_not_expand() {
        let s = Settings::default();
        let mut l = ExpandLogic::default();
        l.update(&s, &Inputs { in_hover_zone: true, ..at(0.0) });
        l.update(&s, &at(0.1));
        assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, ..at(0.2) }), Mode::Collapsed);
        assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, ..at(0.36) }), Mode::Expanded);
    }

    #[test]
    fn hover_disabled_or_button_held() {
        let s = Settings { expand_on_hover: false, ..Default::default() };
        let mut l = ExpandLogic::default();
        for t in 0..10 {
            assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, ..at(t as f64) }), Mode::Collapsed);
        }
        let s = Settings::default();
        let mut l = ExpandLogic::default();
        for t in 0..10 {
            // Dragging a window up to the menu bar doesn't open the shelf.
            assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, button_down: true, ..at(t as f64) }), Mode::Collapsed);
        }
    }

    #[test]
    fn drag_expands_immediately_and_stays_while_dragging_nearby() {
        let s = Settings::default();
        let mut l = ExpandLogic::default();
        assert_eq!(l.update(&s, &Inputs { external_drag: true, button_down: true, ..at(0.0) }), Mode::Collapsed);
        assert_eq!(l.update(&s, &Inputs { external_drag: true, button_down: true, in_drag_zone: true, ..at(0.1) }), Mode::Expanded);
        // Outside the panel itself but still in the zone: stays open.
        assert_eq!(l.update(&s, &Inputs { external_drag: true, in_drag_zone: true, ..at(5.0) }), Mode::Expanded);
        // Clicking elsewhere is ignored mid-drag.
        assert_eq!(l.update(&s, &Inputs { external_drag: true, in_drag_zone: true, mouse_down_outside: true, ..at(5.1) }), Mode::Expanded);
        assert_eq!(l.update(&s, &at(6.0)), Mode::Expanded);
        assert_eq!(l.update(&s, &at(6.5)), Mode::Collapsed);
        let off = Settings { expand_on_drag: false, ..Default::default() };
        let mut l = ExpandLogic::default();
        assert_eq!(l.update(&off, &Inputs { external_drag: true, in_drag_zone: true, ..at(0.0) }), Mode::Collapsed);
    }

    #[test]
    fn click_toggles_and_outside_click_dismisses_without_rehover() {
        let s = Settings { hover_delay: 10.0, ..Default::default() };
        let mut l = ExpandLogic::default();
        assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, clicked_notch: true, ..at(0.0) }), Mode::Expanded);
        assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, in_keep_zone: true, mouse_down_outside: true, ..at(0.5) }), Mode::Collapsed);
        let s = Settings::default();
        // Pointer stays on the notch: no hover re-expansion until it leaves.
        for t in 1..5 {
            assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, ..at(t as f64) }), Mode::Collapsed);
        }
        l.update(&s, &at(6.0));
        l.update(&s, &Inputs { in_hover_zone: true, ..at(7.0) });
        assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, ..at(7.2) }), Mode::Expanded);
    }

    #[test]
    fn pinned_keeps_open_and_peek_times_out() {
        let s = Settings::default();
        let mut l = ExpandLogic::default();
        l.expand();
        assert_eq!(l.update(&s, &Inputs { pinned: true, mouse_down_outside: true, ..at(0.0) }), Mode::Expanded);
        assert_eq!(l.update(&s, &Inputs { pinned: true, ..at(100.0) }), Mode::Expanded);
        l.collapse(false);
        l.peek(200.0, 2.0);
        assert_eq!(l.update(&s, &at(201.0)), Mode::Peek);
        assert_eq!(l.update(&s, &at(202.1)), Mode::Collapsed);
        // Hovering during a peek still expands.
        l.peek(300.0, 2.0);
        l.update(&s, &Inputs { in_hover_zone: true, ..at(300.1) });
        assert_eq!(l.update(&s, &Inputs { in_hover_zone: true, ..at(300.3) }), Mode::Expanded);
        // No peek over an expanded panel.
        l.peek(301.0, 2.0);
        assert_eq!(l.mode(), Mode::Expanded);
    }
}
