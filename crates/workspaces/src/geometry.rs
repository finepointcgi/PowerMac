//! Rectangles, monitors and mapping a saved window frame onto the current
//! monitor layout.
//!
//! Frames are global, top-left-origin coordinates: points on macOS (the
//! Accessibility / CoreGraphics space), physical pixels on Windows (the
//! process is per-monitor DPI aware), as in PowerToys.

use serde::{Deserialize, Serialize};

/// A rectangle (`x`, `y` = top-left corner).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self { x, y, width, height }
    }

    pub fn right(&self) -> i32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }

    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn intersection_area(&self, o: &Rect) -> i64 {
        let w = (self.right().min(o.right()) - self.x.max(o.x)).max(0) as i64;
        let h = (self.bottom().min(o.bottom()) - self.y.max(o.y)).max(0) as i64;
        w * h
    }

    /// Sum of the distances of the four edges (PowerToys' window distance).
    pub fn edge_distance(&self, o: &Rect) -> i64 {
        ((self.x - o.x).abs() + (self.y - o.y).abs() + (self.right() - o.right()).abs() + (self.bottom() - o.bottom()).abs())
            as i64
    }

    /// `true` if every edge is within `tol` of `o`'s.
    pub fn approx_eq(&self, o: &Rect, tol: i32) -> bool {
        (self.x - o.x).abs() <= tol
            && (self.y - o.y).abs() <= tol
            && (self.right() - o.right()).abs() <= tol
            && (self.bottom() - o.bottom()).abs() <= tol
    }

    /// Shrink to fit inside `area`, then move inside it.
    pub fn fit_into(&self, area: &Rect) -> Rect {
        if area.is_empty() {
            return *self;
        }
        let w = self.width.min(area.width).max(1);
        let h = self.height.min(area.height).max(1);
        let x = self.x.clamp(area.x, area.right() - w);
        let y = self.y.clamp(area.y, area.bottom() - h);
        Rect::new(x, y, w, h)
    }
}

/// A display at capture or launch time.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Monitor {
    /// Stable id, the same string FancyZones uses as the key of its applied
    /// layouts (`VVVV-MMMM-SSSSSSSS` from the EDID vendor / model / serial on
    /// macOS, the device interface path on Windows).
    pub id: String,
    pub name: String,
    /// 0-based position in the system's monitor list (primary first on
    /// macOS); PowerToys' `monitor-number` on import.
    pub number: u32,
    pub frame: Rect,
    /// Usable area (without menu bar / Dock / taskbar).
    pub work_area: Rect,
    pub scale: f64,
    pub primary: bool,
}

/// How a window is restored when the monitor it was saved on is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MissingMonitor {
    /// Keep the window's relative position and size, scaled onto the
    /// primary display's work area.
    #[default]
    ScaleToPrimary,
    /// PowerToys' behaviour: fill the primary display's work area and
    /// minimize the window.
    PrimaryMinimized,
}

/// Where a saved window goes on the current monitors.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapped {
    pub frame: Rect,
    /// Id of the current monitor the window lands on (if any).
    pub monitor_id: Option<String>,
    /// The saved monitor is missing; the fallback was used.
    pub fallback: bool,
    /// Minimize regardless of the saved state (PrimaryMinimized fallback).
    pub force_minimized: bool,
}

/// The current monitor that corresponds to a saved one: the same id, else
/// one with the same name and size (ids can change when a display reports
/// no serial number), else `None`.
pub fn find_monitor<'a>(saved: &Monitor, current: &'a [Monitor]) -> Option<&'a Monitor> {
    current.iter().find(|m| m.id == saved.id).or_else(|| {
        current.iter().find(|m| {
            !saved.name.is_empty()
                && m.name == saved.name
                && m.frame.width == saved.frame.width
                && m.frame.height == saved.frame.height
                && !current.iter().any(|o| o.id == saved.id)
        })
    })
}

pub fn primary(current: &[Monitor]) -> Option<&Monitor> {
    current.iter().find(|m| m.primary).or_else(|| current.first())
}

/// Map `r` from `from` to `to` proportionally (relative to the work areas).
pub fn scale_between(r: &Rect, from: &Rect, to: &Rect) -> Rect {
    if from.is_empty() || to.is_empty() {
        return *r;
    }
    let sx = to.width as f64 / from.width as f64;
    let sy = to.height as f64 / from.height as f64;
    let x = to.x as f64 + (r.x - from.x) as f64 * sx;
    let y = to.y as f64 + (r.y - from.y) as f64 * sy;
    Rect::new(
        x.round() as i32,
        y.round() as i32,
        (r.width as f64 * sx).round() as i32,
        (r.height as f64 * sy).round() as i32,
    )
}

/// The monitor a frame is mostly on (largest overlap, else nearest center).
pub fn monitor_for_frame<'a>(r: &Rect, monitors: &'a [Monitor]) -> Option<&'a Monitor> {
    let best = monitors.iter().max_by_key(|m| m.frame.intersection_area(r));
    if let Some(b) = best {
        if b.frame.intersection_area(r) > 0 {
            return Some(b);
        }
    }
    let (cx, cy) = r.center();
    monitors.iter().min_by_key(|m| {
        let (mx, my) = m.frame.center();
        ((mx - cx) as i64).pow(2) + ((my - cy) as i64).pow(2)
    })
}

/// Map a saved frame (captured on `saved_monitor`, which may be `None` for
/// imported data without monitor info) onto the current monitors.
///
/// * Same monitor, same geometry: the frame is kept as is.
/// * Same monitor, different resolution or arrangement: the frame is scaled
///   from the saved work area to the current one.
/// * Monitor gone: [`MissingMonitor`] decides (scaled onto the primary
///   display, or PowerToys' "primary work area, minimized").
///
/// `zoomed` windows fill the target work area. The result always lies
/// inside the target work area.
pub fn map_frame(
    frame: &Rect,
    saved_monitor: Option<&Monitor>,
    zoomed: bool,
    current: &[Monitor],
    missing: MissingMonitor,
) -> Mapped {
    let Some(prim) = primary(current) else {
        return Mapped { frame: *frame, monitor_id: None, fallback: false, force_minimized: false };
    };
    let Some(saved) = saved_monitor else {
        // No monitor info: keep the frame if it is visible somewhere.
        let target = monitor_for_frame(frame, current).unwrap_or(prim);
        let r = if zoomed { target.work_area } else { frame.fit_into(&target.work_area) };
        return Mapped { frame: r, monitor_id: Some(target.id.clone()), fallback: false, force_minimized: false };
    };
    if let Some(target) = find_monitor(saved, current) {
        let r = if zoomed {
            target.work_area
        } else if saved.work_area == target.work_area || saved.work_area.is_empty() {
            *frame
        } else if saved.work_area.width == target.work_area.width && saved.work_area.height == target.work_area.height {
            // Same size, the arrangement changed: translate.
            Rect::new(
                frame.x - saved.work_area.x + target.work_area.x,
                frame.y - saved.work_area.y + target.work_area.y,
                frame.width,
                frame.height,
            )
        } else {
            scale_between(frame, &saved.work_area, &target.work_area)
        };
        return Mapped {
            frame: r.fit_into(&target.work_area),
            monitor_id: Some(target.id.clone()),
            fallback: false,
            force_minimized: false,
        };
    }
    match missing {
        MissingMonitor::PrimaryMinimized => Mapped {
            frame: prim.work_area,
            monitor_id: Some(prim.id.clone()),
            fallback: true,
            force_minimized: true,
        },
        MissingMonitor::ScaleToPrimary => {
            let from = if saved.work_area.is_empty() { saved.frame } else { saved.work_area };
            let r = if zoomed { prim.work_area } else { scale_between(frame, &from, &prim.work_area) };
            Mapped {
                frame: r.fit_into(&prim.work_area),
                monitor_id: Some(prim.id.clone()),
                fallback: true,
                force_minimized: false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(id: &str, frame: Rect, work: Rect, primary: bool) -> Monitor {
        Monitor { id: id.into(), name: format!("Display {id}"), number: 0, frame, work_area: work, scale: 2.0, primary }
    }

    fn laptop() -> Monitor {
        mon("A", Rect::new(0, 0, 1512, 982), Rect::new(0, 33, 1512, 949), true)
    }

    fn external_right() -> Monitor {
        mon("B", Rect::new(1512, 0, 2560, 1440), Rect::new(1512, 25, 2560, 1415), false)
    }

    #[test]
    fn same_layout_keeps_frame() {
        let cur = vec![laptop(), external_right()];
        let f = Rect::new(1600, 100, 1200, 800);
        let m = map_frame(&f, Some(&external_right()), false, &cur, MissingMonitor::ScaleToPrimary);
        assert_eq!(m.frame, f);
        assert_eq!(m.monitor_id.as_deref(), Some("B"));
        assert!(!m.fallback);
    }

    #[test]
    fn rearranged_monitor_translates() {
        // B moved from the right of A to the left of it.
        let mut b_now = external_right();
        b_now.frame = Rect::new(-2560, 0, 2560, 1440);
        b_now.work_area = Rect::new(-2560, 25, 2560, 1415);
        let cur = vec![laptop(), b_now];
        let f = Rect::new(1600, 100, 1200, 800);
        let m = map_frame(&f, Some(&external_right()), false, &cur, MissingMonitor::ScaleToPrimary);
        assert_eq!(m.frame, Rect::new(1600 - 1512 - 2560, 100, 1200, 800));
    }

    #[test]
    fn resolution_change_scales() {
        let mut b_now = external_right();
        b_now.frame = Rect::new(1512, 0, 1920, 1080);
        b_now.work_area = Rect::new(1512, 25, 1920, 1055);
        let cur = vec![laptop(), b_now];
        // left half of the saved external
        let f = Rect::new(1512, 25, 1280, 1415);
        let m = map_frame(&f, Some(&external_right()), false, &cur, MissingMonitor::ScaleToPrimary);
        assert_eq!(m.frame, Rect::new(1512, 25, 960, 1055));
    }

    #[test]
    fn missing_monitor_scales_to_primary() {
        let cur = vec![laptop()];
        let f = Rect::new(1512, 25, 1280, 1415); // left half of B
        let m = map_frame(&f, Some(&external_right()), false, &cur, MissingMonitor::ScaleToPrimary);
        assert!(m.fallback);
        assert!(!m.force_minimized);
        assert_eq!(m.monitor_id.as_deref(), Some("A"));
        assert_eq!(m.frame, Rect::new(0, 33, 756, 949));
    }

    #[test]
    fn missing_monitor_powertoys_strategy() {
        let cur = vec![laptop()];
        let f = Rect::new(1600, 100, 1200, 800);
        let m = map_frame(&f, Some(&external_right()), false, &cur, MissingMonitor::PrimaryMinimized);
        assert!(m.fallback && m.force_minimized);
        assert_eq!(m.frame, laptop().work_area);
    }

    #[test]
    fn zoomed_fills_work_area() {
        let mut b_now = external_right();
        b_now.frame = Rect::new(1512, 0, 3840, 2160);
        b_now.work_area = Rect::new(1512, 25, 3840, 2135);
        let cur = vec![laptop(), b_now.clone()];
        let m = map_frame(&external_right().work_area, Some(&external_right()), true, &cur, MissingMonitor::ScaleToPrimary);
        assert_eq!(m.frame, b_now.work_area);
    }

    #[test]
    fn result_is_inside_work_area() {
        let cur = vec![laptop()];
        // partly off-screen on the saved monitor
        let f = Rect::new(-200, 900, 2000, 400);
        let m = map_frame(&f, Some(&laptop()), false, &cur, MissingMonitor::ScaleToPrimary);
        assert_eq!(m.frame, Rect::new(0, 582, 1512, 400));
    }

    #[test]
    fn id_change_matches_by_name_and_size() {
        let mut b_now = external_right();
        b_now.id = "display-5".into();
        let cur = vec![laptop(), b_now];
        let f = Rect::new(1600, 100, 1200, 800);
        let m = map_frame(&f, Some(&external_right()), false, &cur, MissingMonitor::ScaleToPrimary);
        assert!(!m.fallback);
        assert_eq!(m.monitor_id.as_deref(), Some("display-5"));
        assert_eq!(m.frame, f);
    }

    #[test]
    fn no_saved_monitor_keeps_visible_frame() {
        let cur = vec![laptop(), external_right()];
        let f = Rect::new(1700, 200, 800, 600);
        let m = map_frame(&f, None, false, &cur, MissingMonitor::ScaleToPrimary);
        assert_eq!(m.frame, f);
        assert_eq!(m.monitor_id.as_deref(), Some("B"));
    }

    #[test]
    fn monitor_for_frame_prefers_overlap() {
        let cur = vec![laptop(), external_right()];
        assert_eq!(monitor_for_frame(&Rect::new(1400, 100, 800, 600), &cur).unwrap().id, "B");
        assert_eq!(monitor_for_frame(&Rect::new(-5000, -5000, 10, 10), &cur).unwrap().id, "A");
    }
}
