//! Where the "preview of the selected window" goes (platform independent).
//!
//! A visible window is previewed at its real position and size, on the
//! screen it's on. Windows that can't be seen where they are — minimized,
//! on another Space / desktop, of a hidden app, off-screen, or (with
//! `only_current_screen`) on another screen — are shown centered on the
//! switcher's screen at a comfortable size, with a badge saying why.

use crate::model::{RectF, WindowInfo};

/// Why a preview isn't at the window's real position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    Minimized,
    OtherSpace,
    Hidden,
    OtherScreen,
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Badge::Minimized => "Minimized",
            Badge::OtherSpace => "On another desktop",
            Badge::Hidden => "Hidden",
            Badge::OtherScreen => "On another screen",
        }
    }
}

/// Result of [`place`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// The screen the preview is drawn on (global coordinates).
    pub screen: RectF,
    /// The preview's frame (global coordinates).
    pub frame: RectF,
    /// Why it's centered instead of at the window's real position.
    pub badge: Option<Badge>,
    /// At the window's real position and size.
    pub real: bool,
}

impl Placement {
    pub fn at_real_position(&self) -> bool {
        self.real
    }
}

/// Largest share of the screen a centered preview may cover.
pub const CENTERED_MAX_W: f64 = 0.62;
pub const CENTERED_MAX_H: f64 = 0.62;
/// Smallest centered preview width (points), if the screen allows.
pub const CENTERED_MIN_W: f64 = 360.0;

/// Place the preview of `w`. `hud_screen` is the screen the switcher is on;
/// `screens` all screens (global frames).
pub fn place(w: &WindowInfo, hud_screen: RectF, screens: &[RectF], only_current_screen: bool) -> Placement {
    let badge = if w.minimized {
        Some(Badge::Minimized)
    } else if w.app_hidden {
        Some(Badge::Hidden)
    } else if w.other_space {
        Some(Badge::OtherSpace)
    } else {
        None
    };
    if badge.is_none() && w.frame.w >= 1.0 && w.frame.h >= 1.0 {
        if let Some(i) = w.screen_index(screens) {
            let screen = screens[i];
            let same = screen == hud_screen || (screens.len() <= 1);
            if same || !only_current_screen {
                return Placement { screen, frame: w.frame, badge: None, real: true };
            }
            return centered(w, hud_screen, Some(Badge::OtherScreen));
        }
    }
    centered(w, hud_screen, badge)
}

fn centered(w: &WindowInfo, screen: RectF, badge: Option<Badge>) -> Placement {
    let aspect = w.aspect();
    // The window's own size, but not bigger than a share of the screen,
    // and not tiny either.
    let (mut fw, mut fh) = if w.frame.w >= 1.0 && w.frame.h >= 1.0 { (w.frame.w, w.frame.h) } else { (screen.w * 0.5, screen.w * 0.5 / aspect) };
    let max_w = screen.w * CENTERED_MAX_W;
    let max_h = screen.h * CENTERED_MAX_H;
    let k = (max_w / fw).min(max_h / fh).min(1.0);
    fw *= k;
    fh *= k;
    if fw < CENTERED_MIN_W {
        let k = (CENTERED_MIN_W / fw).min(max_h / fh);
        if k > 1.0 {
            fw *= k;
            fh *= k;
        }
    }
    let (fw, fh) = (fw.round(), fh.round());
    let (cx, cy) = screen.center();
    let frame = RectF::new((cx - fw / 2.0).round(), (cy - fh / 2.0).round(), fw, fh);
    Placement { screen, frame, badge, real: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: RectF = RectF::new(0.0, 0.0, 1512.0, 982.0);
    const EXT: RectF = RectF::new(1512.0, -200.0, 2560.0, 1440.0);

    fn win(frame: RectF) -> WindowInfo {
        WindowInfo { id: 7, pid: 1, title: "Notes".into(), frame, ..Default::default() }
    }

    #[test]
    fn visible_window_previews_at_its_real_frame() {
        let w = win(RectF::new(120.0, 80.0, 900.0, 600.0));
        let p = place(&w, MAIN, &[MAIN, EXT], false);
        assert_eq!(p, Placement { screen: MAIN, frame: w.frame, badge: None, real: true });
        assert!(p.at_real_position());
        // On the second screen: drawn there, at its real frame.
        let w2 = win(RectF::new(1700.0, 0.0, 1200.0, 800.0));
        let p = place(&w2, MAIN, &[MAIN, EXT], false);
        assert_eq!((p.screen, p.frame, p.badge), (EXT, w2.frame, None));
    }

    #[test]
    fn only_current_screen_centers_windows_of_other_screens() {
        let w2 = win(RectF::new(1700.0, 0.0, 1200.0, 800.0));
        let p = place(&w2, MAIN, &[MAIN, EXT], true);
        assert_eq!(p.screen, MAIN);
        assert_eq!(p.badge, Some(Badge::OtherScreen));
        let (cx, cy) = p.frame.center();
        assert!((cx - 756.0).abs() <= 1.0 && (cy - 491.0).abs() <= 1.0);
        // A window on the switcher's own screen is still at its real place.
        let w = win(RectF::new(10.0, 40.0, 500.0, 400.0));
        assert!(place(&w, MAIN, &[MAIN, EXT], true).at_real_position());
    }

    #[test]
    fn minimized_and_other_space_are_centered_with_a_badge() {
        let big = win(RectF::new(0.0, 25.0, 1512.0, 957.0));
        let mut m = big.clone();
        m.minimized = true;
        let p = place(&m, MAIN, &[MAIN], false);
        assert_eq!(p.badge, Some(Badge::Minimized));
        assert_eq!(p.screen, MAIN);
        // Scaled down to the centered maximum, aspect kept, centered.
        assert!(p.frame.w <= MAIN.w * CENTERED_MAX_W + 0.5 && p.frame.h <= MAIN.h * CENTERED_MAX_H + 0.5);
        assert!((p.frame.w / p.frame.h - big.frame.w / big.frame.h).abs() < 0.01);
        let (cx, cy) = p.frame.center();
        assert!((cx - 756.0).abs() <= 1.0 && (cy - 491.0).abs() <= 1.0);

        let mut s = win(RectF::new(1600.0, 100.0, 800.0, 500.0));
        s.other_space = true;
        let p = place(&s, MAIN, &[MAIN, EXT], false);
        assert_eq!(p.badge, Some(Badge::OtherSpace));
        assert_eq!(p.screen, MAIN, "centered on the switcher's screen, not the window's");
        assert_eq!((p.frame.w, p.frame.h), (800.0, 500.0), "small enough: real size");

        let mut h = win(RectF::new(100.0, 100.0, 600.0, 400.0));
        h.app_hidden = true;
        assert_eq!(place(&h, MAIN, &[MAIN], false).badge, Some(Badge::Hidden));
        assert_eq!(Badge::OtherSpace.label(), "On another desktop");
    }

    #[test]
    fn tiny_or_unknown_frames_get_a_readable_size() {
        let mut t = win(RectF::new(0.0, 0.0, 200.0, 120.0));
        t.minimized = true;
        let p = place(&t, MAIN, &[MAIN], false);
        assert!(p.frame.w >= CENTERED_MIN_W - 0.5, "{:?}", p.frame);
        // Off-screen window (all screens miss it): centered, no badge.
        let off = win(RectF::new(-9000.0, -9000.0, 800.0, 600.0));
        let p = place(&off, MAIN, &[MAIN], false);
        assert_eq!(p.badge, None);
        assert!(!p.at_real_position());
        assert!(MAIN.contains(p.frame.x, p.frame.y));
        // No frame at all.
        let mut z = win(RectF::default());
        z.minimized = true;
        let p = place(&z, MAIN, &[MAIN], false);
        assert!(p.frame.w > 0.0 && p.frame.h > 0.0);
    }
}
