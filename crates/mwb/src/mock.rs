//! In-memory input backend used by tests (and handy for headless experiments).

use std::sync::{Arc, Mutex};

use pm_input::{CaptureCallback, CaptureHandle, Disposition, InputBackend, InputEvent, KeyEvent, MouseEvent, Rect};

#[derive(Debug, Clone, PartialEq)]
pub enum Injected {
    Mouse(MouseEvent),
    Key(KeyEvent),
}

#[derive(Default)]
struct Inner {
    cursor: (i32, i32),
    visible: bool,
    locked: bool,
    injected: Vec<Injected>,
    cb: Option<CaptureCallback>,
}

/// Mock backend: records injections, lets tests feed "captured" events.
pub struct MockBackend {
    screens: Vec<Rect>,
    inner: Arc<Mutex<Inner>>,
}

struct MockCapture(Arc<Mutex<Inner>>);
impl CaptureHandle for MockCapture {}
impl Drop for MockCapture {
    fn drop(&mut self) {
        self.0.lock().unwrap().cb = None;
    }
}

impl MockBackend {
    pub fn new(screens: Vec<Rect>) -> Arc<Self> {
        let c = screens.first().map(|r| (r.left + r.width() / 2, r.top + r.height() / 2)).unwrap_or((0, 0));
        Arc::new(MockBackend { screens, inner: Arc::new(Mutex::new(Inner { cursor: c, visible: true, ..Default::default() })) })
    }

    /// Single 1920x1080 screen.
    pub fn hd() -> Arc<Self> {
        Self::new(vec![Rect { left: 0, top: 0, right: 1920, bottom: 1080 }])
    }

    /// Feed a captured event as the OS hook would. Returns the engine's disposition.
    /// For `Move`, the cursor follows unless swallowed or locked.
    pub fn fire(&self, ev: InputEvent) -> Disposition {
        let cb = self.inner.lock().unwrap().cb.clone();
        let d = match cb {
            Some(cb) => cb(&ev),
            None => Disposition::Pass,
        };
        if let (InputEvent::Mouse(MouseEvent::Move { x, y, .. }), Disposition::Pass) = (ev, d) {
            let mut i = self.inner.lock().unwrap();
            if !i.locked {
                i.cursor = (x, y);
            }
        }
        d
    }

    /// Simulate a physical move by (dx, dy) from the current cursor position.
    pub fn move_by(&self, dx: i32, dy: i32) -> Disposition {
        let (cx, cy) = self.inner.lock().unwrap().cursor;
        self.fire(InputEvent::Mouse(MouseEvent::Move { x: cx + dx, y: cy + dy, dx, dy }))
    }

    pub fn injected(&self) -> Vec<Injected> {
        self.inner.lock().unwrap().injected.clone()
    }
    pub fn clear_injected(&self) {
        self.inner.lock().unwrap().injected.clear()
    }
    pub fn cursor(&self) -> (i32, i32) {
        self.inner.lock().unwrap().cursor
    }
    pub fn cursor_visible(&self) -> bool {
        self.inner.lock().unwrap().visible
    }
    pub fn cursor_locked(&self) -> bool {
        self.inner.lock().unwrap().locked
    }
    pub fn capturing(&self) -> bool {
        self.inner.lock().unwrap().cb.is_some()
    }
}

impl InputBackend for MockBackend {
    fn start_capture(&self, cb: CaptureCallback) -> anyhow::Result<Box<dyn CaptureHandle>> {
        self.inner.lock().unwrap().cb = Some(cb);
        Ok(Box::new(MockCapture(self.inner.clone())))
    }
    fn inject_mouse(&self, ev: MouseEvent) -> anyhow::Result<()> {
        let mut i = self.inner.lock().unwrap();
        if let MouseEvent::Move { x, y, .. } = ev {
            i.cursor = (x, y);
        }
        i.injected.push(Injected::Mouse(ev));
        Ok(())
    }
    fn inject_key(&self, ev: KeyEvent) -> anyhow::Result<()> {
        self.inner.lock().unwrap().injected.push(Injected::Key(ev));
        Ok(())
    }
    fn set_cursor_pos(&self, x: i32, y: i32) -> anyhow::Result<()> {
        self.inner.lock().unwrap().cursor = (x, y);
        Ok(())
    }
    fn cursor_pos(&self) -> anyhow::Result<(i32, i32)> {
        Ok(self.inner.lock().unwrap().cursor)
    }
    fn set_cursor_visible(&self, visible: bool) -> anyhow::Result<()> {
        self.inner.lock().unwrap().visible = visible;
        Ok(())
    }
    fn set_cursor_locked(&self, locked: bool) -> anyhow::Result<()> {
        self.inner.lock().unwrap().locked = locked;
        Ok(())
    }
    fn screens(&self) -> anyhow::Result<Vec<Rect>> {
        Ok(self.screens.clone())
    }
    fn has_permissions(&self, _prompt: bool) -> bool {
        true
    }
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn f<T: Send + Sync>() {}
    f::<MockBackend>();
    let _ = InputEvent::Key(KeyEvent { vk: 0, down: true, extended: false });
}
