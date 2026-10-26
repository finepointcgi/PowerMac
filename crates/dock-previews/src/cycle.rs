//! "Dock click cycles windows": what a click on the Dock icon of an app
//! does when [`crate::Config::click_to_cycle`] is on, Windows-taskbar
//! style. Pure decision; `None` means "let the Dock handle the click".

/// A window of the clicked app, as far as the decision is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Win {
    pub id: u64,
    pub minimized: bool,
}

/// What to do instead of the Dock's own click handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleAction {
    /// Minimize this (the app's only visible) window.
    Minimize(u64),
    /// Bring this window to the front (un-minimizing it if needed).
    Focus(u64),
}

/// Decide for an app with `windows` (on the current Space, in the app's
/// window order, front first) whose focused window is `focused`.
///
/// * The app isn't frontmost → `None` (the Dock activates it).
/// * No windows, or all minimized → `None` (the Dock restores one / opens a
///   new window as usual).
/// * One window, visible → minimize it.
/// * Several → focus the next one after the focused window in a stable
///   (id) order, wrapping around; minimized ones are included and restored.
pub fn decide(app_is_front: bool, windows: &[Win], focused: Option<u64>) -> Option<CycleAction> {
    if !app_is_front || windows.iter().all(|w| w.minimized) {
        return None;
    }
    if windows.len() == 1 {
        return Some(CycleAction::Minimize(windows[0].id));
    }
    let mut ids: Vec<u64> = windows.iter().map(|w| w.id).collect();
    ids.sort_unstable();
    ids.dedup();
    let cur = focused.or_else(|| windows.iter().find(|w| !w.minimized).map(|w| w.id));
    let next = match cur.and_then(|c| ids.iter().position(|&i| i == c)) {
        Some(k) => ids[(k + 1) % ids.len()],
        None => ids[0],
    };
    Some(CycleAction::Focus(next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: u64, minimized: bool) -> Win {
        Win { id, minimized }
    }

    #[test]
    fn not_front_or_nothing_visible_passes_through() {
        assert_eq!(decide(false, &[w(1, false)], Some(1)), None);
        assert_eq!(decide(true, &[], None), None);
        assert_eq!(decide(true, &[w(1, true), w(2, true)], None), None);
    }

    #[test]
    fn single_window_minimizes() {
        assert_eq!(decide(true, &[w(7, false)], Some(7)), Some(CycleAction::Minimize(7)));
        assert_eq!(decide(true, &[w(7, false)], None), Some(CycleAction::Minimize(7)));
    }

    #[test]
    fn several_windows_cycle_in_id_order() {
        // z-order front first: 30 (focused), 10, 20.
        let ws = [w(30, false), w(10, false), w(20, true)];
        assert_eq!(decide(true, &ws, Some(30)), Some(CycleAction::Focus(10)));
        assert_eq!(decide(true, &ws, Some(10)), Some(CycleAction::Focus(20)));
        assert_eq!(decide(true, &ws, Some(20)), Some(CycleAction::Focus(30)));
        // Focused window unknown (e.g. a panel): the frontmost visible counts.
        assert_eq!(decide(true, &ws, None), Some(CycleAction::Focus(10)));
        // Focus on a window not in the list: start at the first.
        assert_eq!(decide(true, &ws, Some(99)), Some(CycleAction::Focus(10)));
    }
}
