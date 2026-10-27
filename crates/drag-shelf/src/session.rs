//! Is a drag carrying files / links / text / images under way?
//!
//! macOS has no notification for "a drag session started", but every drag
//! source writes its items to the drag pasteboard (`NSPasteboardNameDrag`),
//! which bumps its change count. So: remember the count while the button is
//! up; while it is held, a different count whose types include something a
//! shelf takes means a drag of shelf-able content. Selecting text or moving
//! a window never writes the drag pasteboard, so shaking then does nothing.

use pm_shelf_core::uti;

/// What [`DragWatch::session`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    /// No drag, or one that carries nothing a shelf takes.
    None,
    /// Something else is dragging files / links / text / images.
    Foreign,
    /// One of our own shelves is dragging items out (shaking then splits
    /// them onto a new shelf).
    Own,
}

impl Session {
    pub fn is_drag(self) -> bool {
        self != Session::None
    }
}

/// Tracks the drag pasteboard's change count between drags.
#[derive(Clone, Debug, Default)]
pub struct DragWatch {
    /// The change count while the button was last up (`None` until known).
    baseline: Option<isize>,
    /// A shelf is dragging items out (until its session ends).
    own: bool,
}

impl DragWatch {
    pub fn new(count: isize) -> Self {
        DragWatch { baseline: Some(count), own: false }
    }

    /// The left button went up (or is up): drags since are new ones.
    pub fn button_up(&mut self, count: isize) {
        self.baseline = Some(count);
    }

    /// A shelf started (`true`) or finished (`false`) dragging items out.
    pub fn set_own_drag(&mut self, on: bool) {
        self.own = on;
    }

    pub fn own_drag(&self) -> bool {
        self.own
    }

    /// The drag under way: `button_down` is the left button now, `count`
    /// and `types` the drag pasteboard's change count and types.
    pub fn session(&self, button_down: bool, count: isize, types: &[String]) -> Session {
        if !button_down {
            return Session::None;
        }
        if self.own {
            return Session::Own;
        }
        match self.baseline {
            Some(b) if b != count && uti::accepts(types) => Session::Foreign,
            _ => Session::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn drag_session_detection() {
        let files = t(&[uti::T_FILE_URL, "com.apple.finder.node"]);
        let mut w = DragWatch::new(10);
        // Button up: never a drag.
        assert_eq!(w.session(false, 11, &files), Session::None);
        // Held, pasteboard unchanged since the button went up: a window
        // move, a text selection, a click and hold.
        assert_eq!(w.session(true, 10, &files), Session::None);
        // Held and the drag pasteboard was written: a file drag.
        assert_eq!(w.session(true, 11, &files), Session::Foreign);
        // Something only a private type: not for the shelf.
        assert_eq!(w.session(true, 11, &t(&["com.example.private"])), Session::None);
        // Text, links, images and promises count.
        for ty in [uti::T_TEXT, uti::T_URL, uti::T_PNG, uti::T_PROMISE] {
            assert_eq!(w.session(true, 12, &t(&[ty])), Session::Foreign, "{ty}");
        }
        // After the drop the button goes up: the next hold starts clean.
        w.button_up(11);
        assert_eq!(w.session(true, 11, &files), Session::None);
        assert!(!w.session(true, 11, &files).is_drag());
        // Unknown baseline (started mid-drag): conservative.
        assert_eq!(DragWatch::default().session(true, 3, &files), Session::None);
    }

    #[test]
    fn own_drags() {
        let mut w = DragWatch::new(1);
        w.set_own_drag(true);
        assert!(w.own_drag());
        assert_eq!(w.session(true, 1, &[]), Session::Own);
        assert_eq!(w.session(false, 1, &[]), Session::None);
        w.set_own_drag(false);
        w.button_up(2);
        assert_eq!(w.session(true, 2, &[]), Session::None);
    }
}
