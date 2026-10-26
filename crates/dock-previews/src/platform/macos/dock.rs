//! Reading the Dock through Accessibility: the Dock process exposes one
//! `AXList` whose children are `AXDockItem`s with a subrole (application,
//! separator, trash, folder, ...), `AXTitle`, `AXURL` (the bundle for
//! apps), `AXIsApplicationRunning` and a frame.

use std::sync::Mutex;

use super::apps;
use super::ax::AxElement;
use super::ffi::display_bounds;
use crate::geom::{Dock, DockItem, ItemKind, RectF};

/// Screens (global, top-left origin), main first.
pub fn screens() -> Vec<RectF> {
    display_bounds().into_iter().map(|r| RectF::new(r.origin.x, r.origin.y, r.size.width, r.size.height)).collect()
}

fn dock_app() -> Option<AxElement> {
    // The Dock restarts now and then (e.g. after changing its settings);
    // re-resolve its pid when reading fails.
    static PID: Mutex<Option<i32>> = Mutex::new(None);
    let mut pid = PID.lock().unwrap_or_else(|e| e.into_inner());
    if pid.is_none() {
        *pid = apps::dock_pid();
    }
    let p = (*pid)?;
    let el = AxElement::application(p);
    if el.elements_attr("AXChildren").is_empty() {
        *pid = apps::dock_pid();
        return pid.map(AxElement::application);
    }
    Some(el)
}

/// A snapshot of the Dock, or `None` if it can't be read (no
/// Accessibility permission, Dock not running).
pub fn read() -> Option<Dock> {
    let app = dock_app()?;
    app.set_timeout(0.25);
    let list = app.elements_attr("AXChildren").into_iter().find(|c| c.role().as_deref() == Some("AXList"))?;
    let list_frame = list.frame()?;
    let mut items = Vec::new();
    for it in list.elements_attr("AXChildren") {
        let Some(frame) = it.frame() else { continue };
        let kind = ItemKind::from_subrole(it.subrole().as_deref().unwrap_or(""));
        let (path, running) = if kind == ItemKind::App {
            (it.path_attr("AXURL").unwrap_or_default(), it.bool_attr("AXIsApplicationRunning").unwrap_or(false))
        } else {
            (String::new(), false)
        };
        items.push(DockItem { kind, title: it.string_attr("AXTitle").unwrap_or_default(), path, running, frame });
    }
    Some(Dock::new(list_frame, items, &screens()))
}
