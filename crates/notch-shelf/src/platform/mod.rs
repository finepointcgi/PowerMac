#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{has_notch, Backend};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub(crate) use self::windows::{has_notch, Backend};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod other;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) use other::{has_notch, Backend};

/// Notched MacBook panels (3024×1964, 3456×2234, 2560×1664, 2880×1864 and
/// their scaled modes) are about 1.54:1; un-notched ones are 16:10.
#[allow(dead_code)]
pub(crate) fn aspect_suggests_notch(w: f64, h: f64) -> bool {
    if h <= 0.0 {
        return false;
    }
    let r = w / h;
    (1.50..1.565).contains(&r)
}

#[cfg(test)]
mod tests {
    use super::aspect_suggests_notch;

    #[test]
    fn notch_aspect_heuristic() {
        for (w, h) in [(3024.0, 1964.0), (3456.0, 2234.0), (2560.0, 1664.0), (2880.0, 1864.0), (1512.0, 982.0), (1728.0, 1117.0)] {
            assert!(aspect_suggests_notch(w, h), "{w}x{h}");
        }
        for (w, h) in [(2560.0, 1600.0), (2880.0, 1800.0), (3072.0, 1920.0), (1920.0, 1080.0), (0.0, 0.0)] {
            assert!(!aspect_suggests_notch(w, h), "{w}x{h}");
        }
    }
}

/// Pick the screens that get a shelf: `(screen index, collapsed rect)`.
/// Index 0 is the main (menu bar) screen.
#[allow(dead_code)]
pub(crate) fn choose_screens(mode: crate::ScreenMode, allow_virtual: bool, screens: &[crate::geometry::ScreenGeom], m: &crate::geometry::Metrics) -> Vec<(usize, crate::geometry::Rect)> {
    use crate::geometry::collapsed_rect;
    let notched: Vec<(usize, crate::geometry::Rect)> =
        screens.iter().enumerate().filter(|(_, s)| s.notch.is_some()).filter_map(|(i, s)| collapsed_rect(s, m, false).map(|r| (i, r))).collect();
    match mode {
        crate::ScreenMode::NotchOnly => {
            if !notched.is_empty() {
                notched
            } else if allow_virtual {
                screens.first().and_then(|s| collapsed_rect(s, m, true)).map(|r| vec![(0, r)]).unwrap_or_default()
            } else {
                vec![]
            }
        }
        crate::ScreenMode::All => screens.iter().enumerate().filter_map(|(i, s)| collapsed_rect(s, m, allow_virtual).map(|r| (i, r))).collect(),
    }
}

#[cfg(test)]
mod choose_tests {
    use super::choose_screens;
    use crate::geometry::{Metrics, Rect, ScreenGeom};
    use crate::ScreenMode;

    #[test]
    fn screen_selection() {
        let m = Metrics::default();
        let builtin = ScreenGeom { frame: Rect::new(0.0, 0.0, 1512.0, 982.0), notch: Some(Rect::new(663.5, 0.0, 185.0, 32.0)), menu_bar_h: 32.0 };
        let ext = ScreenGeom { frame: Rect::new(1512.0, 0.0, 2560.0, 1440.0), notch: None, menu_bar_h: 25.0 };
        // External as main screen, built-in second.
        let both = [ext, builtin];
        assert_eq!(choose_screens(ScreenMode::NotchOnly, false, &both, &m), vec![(1, builtin.notch.unwrap())]);
        assert_eq!(choose_screens(ScreenMode::NotchOnly, true, &both, &m).len(), 1);
        assert_eq!(choose_screens(ScreenMode::All, false, &both, &m), vec![(1, builtin.notch.unwrap())]);
        let all = choose_screens(ScreenMode::All, true, &both, &m);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, 0);
        // Lid closed: only the external.
        assert!(choose_screens(ScreenMode::NotchOnly, false, &[ext], &m).is_empty());
        let v = choose_screens(ScreenMode::NotchOnly, true, &[ext], &m);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].1.center_x(), ext.frame.center_x());
    }
}
