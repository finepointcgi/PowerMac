//! Grid layout editing: a port of the FancyZones editor's `GridData.cs`
//! (split / merge / resizer drag on a list of zones in 0..10000 units, kept
//! convertible to FancyZones' `rows-percentage` / `columns-percentage` /
//! `cell-child-map` format), `MagneticSnap.cs`, and the egui grid editor.

use std::collections::BTreeSet;

use eframe::egui::{self, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, Ui};
use fancyzones::layout::{calculate_grid_zones, GridLayoutInfo};

use crate::widgets::ACCENT;

/// Row / column percentages sum to this (`GridData.Multiplier`).
pub const MULTIPLIER: i32 = 10000;
/// `GridEditor.MinZoneSize`: minimum zone size in pixels (plus 2× spacing).
pub const MIN_ZONE_PX: i32 = 100;
/// `MagneticSnap.MagnetZoneMaxSize`.
const MAGNET_ZONE_MAX: i32 = MULTIPLIER / 12;

/// A zone in grid units (`0..=MULTIPLIER` on both axes; right/bottom exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Zone {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    fn area(&self) -> i64 {
        self.width() as i64 * self.height() as i64
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.left <= x && x < self.right && self.top <= y && y < self.bottom
    }
}

/// `Orientation` of a split line / resizer: `Horizontal` is a horizontal line
/// (zones above and below it), `Vertical` a vertical one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

/// `GridData.Resizer`: one draggable divider segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resizer {
    pub orientation: Orientation,
    /// Zones above / left of the divider.
    pub negative: Vec<usize>,
    /// Zones below / right of the divider.
    pub positive: Vec<usize>,
    /// y (horizontal) or x (vertical) of the divider, in grid units.
    pub pos: i32,
    /// Extent along the divider, in grid units.
    pub start: i32,
    pub end: i32,
}

/// The zones of a grid layout; the index in the list is the FancyZones zone id.
/// Invariant: the zones tile the `MULTIPLIER × MULTIPLIER` square exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridModel {
    zones: Vec<Zone>,
}

fn unique(v: impl IntoIterator<Item = i32>) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for x in v {
        if out.last() != Some(&(x as usize)) {
            out.push(x as usize);
        }
    }
    out
}

fn prefix_sum(v: &[i32]) -> Vec<i32> {
    let mut out = Vec::with_capacity(v.len() + 1);
    let mut s = 0;
    out.push(0);
    for x in v {
        s += x;
        out.push(s);
    }
    out
}

/// `n` parts summing to `MULTIPLIER` (FancyZones' rounding).
pub fn even_percents(n: usize) -> Vec<i32> {
    let n = n.max(1) as i32;
    (0..n).map(|i| MULTIPLIER * (i + 1) / n - MULTIPLIER * i / n).collect()
}

/// Rescale percentages that don't sum to `MULTIPLIER` (hand-edited files).
fn normalize_percents(v: &[i32]) -> Option<Vec<i32>> {
    if v.is_empty() || v.iter().any(|&x| x < 1) {
        return None;
    }
    let sum: i64 = v.iter().map(|&x| x as i64).sum();
    if sum == MULTIPLIER as i64 {
        return Some(v.to_vec());
    }
    let mut acc = 0i64;
    let mut prev = 0i32;
    let mut out = Vec::with_capacity(v.len());
    for &x in v {
        acc += x as i64;
        let edge = (acc * MULTIPLIER as i64 / sum) as i32;
        if edge - prev < 1 {
            return None;
        }
        out.push(edge - prev);
        prev = edge;
    }
    Some(out)
}

impl GridModel {
    /// One zone covering everything.
    pub fn single() -> Self {
        Self { zones: vec![Zone::new(0, 0, MULTIPLIER, MULTIPLIER)] }
    }

    /// `n` columns (what the FancyZones editor starts a new grid layout with).
    pub fn columns(n: usize) -> Self {
        let p = prefix_sum(&even_percents(n));
        Self { zones: p.windows(2).map(|w| Zone::new(w[0], 0, w[1], MULTIPLIER)).collect() }
    }

    /// Zones from raw rects (must tile the square; checked).
    pub fn from_zones(zones: Vec<Zone>) -> Result<Self, String> {
        let m = Self { zones };
        m.check()?;
        Ok(m)
    }

    /// `GridData.ModelToZones`: read FancyZones' grid representation.
    pub fn from_info(info: &GridLayoutInfo) -> Result<Self, String> {
        let rows = info.rows_percents.len();
        let cols = info.columns_percents.len();
        if rows == 0 || cols == 0 {
            return Err("the grid has no rows or columns".into());
        }
        if info.cell_child_map.len() != rows || info.cell_child_map.iter().any(|r| r.len() != cols) {
            return Err("cell-child-map doesn't match the rows/columns".into());
        }
        let rp = normalize_percents(&info.rows_percents).ok_or("invalid rows-percentage")?;
        let cp = normalize_percents(&info.columns_percents).ok_or("invalid columns-percentage")?;
        let map = &info.cell_child_map;
        if map.iter().flatten().any(|&i| i < 0) {
            return Err("negative zone id in cell-child-map".into());
        }
        let n = map.iter().flatten().copied().max().unwrap_or(0) as usize + 1;
        if n > rows * cols {
            return Err("zone ids aren't contiguous".into());
        }
        let mut count = vec![0usize; n];
        let mut row_lo = vec![usize::MAX; n];
        let mut row_hi = vec![0usize; n];
        let mut col_lo = vec![usize::MAX; n];
        let mut col_hi = vec![0usize; n];
        for (r, row) in map.iter().enumerate() {
            for (c, &id) in row.iter().enumerate() {
                let i = id as usize;
                count[i] += 1;
                row_lo[i] = row_lo[i].min(r);
                row_hi[i] = row_hi[i].max(r);
                col_lo[i] = col_lo[i].min(c);
                col_hi[i] = col_hi[i].max(c);
            }
        }
        for i in 0..n {
            if count[i] == 0 {
                return Err(format!("zone {i} is missing from cell-child-map"));
            }
            if count[i] != (row_hi[i] - row_lo[i] + 1) * (col_hi[i] - col_lo[i] + 1) {
                return Err(format!("zone {i} isn't a rectangle"));
            }
        }
        let rps = prefix_sum(&rp);
        let cps = prefix_sum(&cp);
        let zones = (0..n)
            .map(|i| Zone::new(cps[col_lo[i]], rps[row_lo[i]], cps[col_hi[i] + 1], rps[row_hi[i] + 1]))
            .collect();
        Ok(Self { zones })
    }

    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    pub fn len(&self) -> usize {
        self.zones.len()
    }

    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }

    /// The tiling invariant (used by tests and `from_zones`).
    pub fn check(&self) -> Result<(), String> {
        let mut area = 0i64;
        for (i, z) in self.zones.iter().enumerate() {
            if z.left < 0 || z.top < 0 || z.right > MULTIPLIER || z.bottom > MULTIPLIER || z.width() < 1 || z.height() < 1 {
                return Err(format!("zone {i} is out of bounds: {z:?}"));
            }
            area += z.area();
            for (j, o) in self.zones.iter().enumerate().skip(i + 1) {
                if z.left.max(o.left) < z.right.min(o.right) && z.top.max(o.top) < z.bottom.min(o.bottom) {
                    return Err(format!("zones {i} and {j} overlap"));
                }
            }
        }
        if area != MULTIPLIER as i64 * MULTIPLIER as i64 {
            return Err("zones don't cover the whole screen".into());
        }
        Ok(())
    }

    fn coords(&self) -> (Vec<i32>, Vec<i32>) {
        let mut xs: Vec<i32> = self.zones.iter().flat_map(|z| [z.left, z.right]).collect();
        let mut ys: Vec<i32> = self.zones.iter().flat_map(|z| [z.top, z.bottom]).collect();
        xs.sort_unstable();
        xs.dedup();
        ys.sort_unstable();
        ys.dedup();
        (xs, ys)
    }

    /// `GridData.ZonesToModel`: FancyZones' grid representation. Spacing and
    /// sensitivity are the FancyZones defaults; the caller sets them.
    pub fn to_info(&self) -> GridLayoutInfo {
        let (xs, ys) = self.coords();
        let rows = ys.len() - 1;
        let cols = xs.len() - 1;
        let mut map = vec![vec![0i32; cols]; rows];
        for (i, z) in self.zones.iter().enumerate() {
            let r0 = ys.binary_search(&z.top).unwrap_or(0);
            let r1 = ys.binary_search(&z.bottom).unwrap_or(rows);
            let c0 = xs.binary_search(&z.left).unwrap_or(0);
            let c1 = xs.binary_search(&z.right).unwrap_or(cols);
            for row in &mut map[r0..r1] {
                for cell in &mut row[c0..c1] {
                    *cell = i as i32;
                }
            }
        }
        GridLayoutInfo::new(
            ys.windows(2).map(|w| w[1] - w[0]).collect(),
            xs.windows(2).map(|w| w[1] - w[0]).collect(),
            map,
        )
    }

    /// `GridData.ModelToResizers`.
    pub fn resizers(&self) -> Vec<Resizer> {
        let info = self.to_info();
        let g = &info.cell_child_map;
        let (rows, cols) = (info.rows, info.columns);
        let rps = prefix_sum(&info.rows_percents);
        let cps = prefix_sum(&info.columns_percents);
        let mut out = Vec::new();
        for row in 1..rows {
            let mut c = 0;
            while c < cols {
                if g[row - 1][c] != g[row][c] {
                    let mut end = c;
                    while end + 1 < cols && g[row - 1][end + 1] != g[row][end + 1] {
                        end += 1;
                    }
                    out.push(Resizer {
                        orientation: Orientation::Horizontal,
                        negative: unique((c..=end).map(|x| g[row - 1][x])),
                        positive: unique((c..=end).map(|x| g[row][x])),
                        pos: rps[row],
                        start: cps[c],
                        end: cps[end + 1],
                    });
                    c = end + 1;
                } else {
                    c += 1;
                }
            }
        }
        for col in 1..cols {
            let mut r = 0;
            while r < rows {
                if g[r][col - 1] != g[r][col] {
                    let mut end = r;
                    while end + 1 < rows && g[end + 1][col - 1] != g[end + 1][col] {
                        end += 1;
                    }
                    out.push(Resizer {
                        orientation: Orientation::Vertical,
                        negative: unique((r..=end).map(|y| g[y][col - 1])),
                        positive: unique((r..=end).map(|y| g[y][col])),
                        pos: cps[col],
                        start: rps[r],
                        end: rps[end + 1],
                    });
                    r = end + 1;
                } else {
                    r += 1;
                }
            }
        }
        out
    }

    /// Zone containing the grid point (`x`, `y`).
    pub fn zone_at(&self, x: i32, y: i32) -> Option<usize> {
        let (x, y) = (x.clamp(0, MULTIPLIER - 1), y.clamp(0, MULTIPLIER - 1));
        self.zones.iter().position(|z| z.contains(x, y))
    }

    /// Inner column (x) and row (y) boundaries: `MagneticSnap` key points.
    pub fn key_points(&self) -> (Vec<i32>, Vec<i32>) {
        let (xs, ys) = self.coords();
        (xs[1..xs.len() - 1].to_vec(), ys[1..ys.len() - 1].to_vec())
    }

    /// `GridData.CanSplit`. `min` is (min width, min height) in grid units.
    pub fn can_split(&self, zone: usize, pos: i32, o: Orientation, min: (i32, i32)) -> bool {
        let Some(z) = self.zones.get(zone) else { return false };
        match o {
            Orientation::Horizontal => z.top + min.1 <= pos && pos <= z.bottom - min.1,
            Orientation::Vertical => z.left + min.0 <= pos && pos <= z.right - min.0,
        }
    }

    /// `GridData.Split`: the new zones get ids `zone` and `zone + 1`.
    pub fn split(&mut self, zone: usize, pos: i32, o: Orientation, min: (i32, i32)) -> bool {
        if !self.can_split(zone, pos, o, min) {
            return false;
        }
        let z = self.zones[zone];
        let (mut a, mut b) = (z, z);
        match o {
            Orientation::Horizontal => {
                a.bottom = pos;
                b.top = pos;
            }
            Orientation::Vertical => {
                a.right = pos;
                b.left = pos;
            }
        }
        self.zones[zone] = a;
        self.zones.insert(zone + 1, b);
        true
    }

    /// Allowed `delta` range for dragging `r` (`GridData.CanDrag`).
    pub fn drag_range(&self, r: &Resizer, min: (i32, i32)) -> (i32, i32) {
        let (size, min): (fn(&Zone) -> i32, i32) = match r.orientation {
            Orientation::Vertical => (Zone::width, min.0),
            Orientation::Horizontal => (Zone::height, min.1),
        };
        let lo = r.negative.iter().filter_map(|&i| self.zones.get(i)).map(|z| min - size(z)).max().unwrap_or(0);
        let hi = r.positive.iter().filter_map(|&i| self.zones.get(i)).map(|z| size(z) - min).min().unwrap_or(0);
        if lo > hi {
            (0, 0)
        } else {
            (lo.min(0), hi.max(0))
        }
    }

    /// `GridData.Drag`, but clamping `delta` to the allowed range instead of
    /// ignoring the drag. Returns the delta applied.
    pub fn drag(&mut self, r: &Resizer, delta: i32, min: (i32, i32)) -> i32 {
        let (lo, hi) = self.drag_range(r, min);
        let d = delta.clamp(lo, hi);
        if d == 0 {
            return 0;
        }
        for &i in &r.positive {
            match r.orientation {
                Orientation::Horizontal => self.zones[i].top += d,
                Orientation::Vertical => self.zones[i].left += d,
            }
        }
        for &i in &r.negative {
            match r.orientation {
                Orientation::Horizontal => self.zones[i].bottom += d,
                Orientation::Vertical => self.zones[i].right += d,
            }
        }
        d
    }

    /// `GridData.ComputeClosure`: the smallest set of zones containing
    /// `indices` whose union is a rectangle, and that rectangle.
    pub fn merge_closure(&self, indices: &[usize]) -> (Vec<usize>, Zone) {
        let mut it = indices.iter().filter_map(|&i| self.zones.get(i));
        let Some(first) = it.next() else { return (vec![], Zone::new(0, 0, 0, 0)) };
        let mut b = *first;
        let extend = |b: &mut Zone, z: &Zone| {
            b.left = b.left.min(z.left);
            b.top = b.top.min(z.top);
            b.right = b.right.max(z.right);
            b.bottom = b.bottom.max(z.bottom);
        };
        for z in it {
            extend(&mut b, z);
        }
        loop {
            let mut changed = false;
            for z in &self.zones {
                let w = (b.right.min(z.right) - b.left.max(z.left)).max(0) as i64;
                let h = (b.bottom.min(z.bottom) - b.top.max(z.top)).max(0) as i64;
                let cut = w * h;
                if cut != 0 && cut != z.area() {
                    extend(&mut b, z);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let inside = self
            .zones
            .iter()
            .enumerate()
            .filter(|(_, z)| b.left <= z.left && z.right <= b.right && b.top <= z.top && z.bottom <= b.bottom)
            .map(|(i, _)| i)
            .collect();
        (inside, b)
    }

    /// Whether `indices` (as a set) already form a rectangle.
    pub fn is_rectangle(&self, indices: &[usize]) -> bool {
        let want: BTreeSet<usize> = indices.iter().copied().filter(|&i| i < self.zones.len()).collect();
        let (closure, _) = self.merge_closure(indices);
        !want.is_empty() && closure.into_iter().collect::<BTreeSet<_>>() == want
    }

    /// `GridData.DoMerge`: replace the closure of `indices` by one zone (whose
    /// id is the lowest id merged). Returns `false` if nothing was merged.
    pub fn merge(&mut self, indices: &[usize]) -> bool {
        let (closure, rect) = self.merge_closure(indices);
        if closure.len() < 2 {
            return false;
        }
        let at = closure[0];
        let set: BTreeSet<usize> = closure.into_iter().collect();
        let mut i = 0;
        self.zones.retain(|_| {
            let keep = !set.contains(&i);
            i += 1;
            keep
        });
        self.zones.insert(at.min(self.zones.len()), rect);
        true
    }
}

/// `MagneticSnap.PixelToDataWithSnapping` with the position already in grid
/// units: snaps `data` towards the nearest key point in `(low, high)`.
pub fn magnetic_snap(key_points: &[i32], data: i32, low: i32, high: i32) -> i32 {
    let kp: Vec<i32> = key_points.iter().copied().filter(|&x| low < x && x < high).collect();
    let sizes: Vec<i32> = (0..kp.len())
        .map(|i| {
            let prev = if i == 0 { low } else { kp[i - 1] };
            let next = if i == kp.len() - 1 { high } else { kp[i + 1] };
            (kp[i] - prev).min((next - kp[i]).min(MAGNET_ZONE_MAX)) / 2
        })
        .collect();
    let data = data.clamp(low, high);
    let result = match (0..kp.len()).find(|&i| (data - kp[i]).abs() <= sizes[i]) {
        None => data,
        Some(i) => {
            let dead = (sizes[i] + 1) / 2;
            if (data - kp[i]).abs() <= dead {
                kp[i]
            } else if data < kp[i] {
                data + (data - (kp[i] - sizes[i]))
            } else {
                data - ((kp[i] + sizes[i]) - data)
            }
        }
    };
    result.clamp(low, high)
}

/// Minimum zone size in grid units for a `work` (w, h) px screen
/// (`GridEditor`: `MinZoneSize + 2 * spacing` pixels).
pub fn min_zone_size(work: (i32, i32), spacing: i32) -> (i32, i32) {
    let px = MIN_ZONE_PX + 2 * spacing.max(0);
    let conv = |size: i32| ((MULTIPLIER as i64 * px as i64) / size.max(1) as i64).clamp(1, MULTIPLIER as i64 / 2) as i32;
    (conv(work.0), conv(work.1))
}

// ---------------------------------------------------------------------------
// UI

#[derive(Debug, Clone)]
enum Drag {
    /// Dragging a divider: the model as it was when the drag started.
    Resizer { base: GridModel, resizer: Resizer, start: Pos2 },
    /// Pressed inside a zone; becomes a click (split) or a merge selection.
    Press { zone: usize, start: Pos2, merging: bool },
}

/// The grid zone editor (state for one editing session).
#[derive(Debug, Clone)]
pub struct GridEditor {
    pub model: GridModel,
    pub show_spacing: bool,
    pub spacing: i32,
    pub sensitivity_radius: i32,
    /// Work area (w, h) in pixels: the aspect ratio and minimum zone size.
    pub work: (i32, i32),
    drag: Option<Drag>,
    /// Zones selected for merging (a closed rectangle) and where to show the
    /// Merge button.
    pub merge_selection: Vec<usize>,
    merge_at: Option<Pos2>,
    /// Where the editor was last drawn.
    pub last_rect: Option<Rect>,
}

impl GridEditor {
    pub fn new(info: &GridLayoutInfo, work: (i32, i32)) -> Self {
        Self {
            model: GridModel::from_info(info).unwrap_or_else(|e| {
                log::warn!("grid layout can't be edited as is ({e}); starting over");
                GridModel::columns(3)
            }),
            show_spacing: info.show_spacing,
            spacing: info.spacing,
            sensitivity_radius: info.sensitivity_radius,
            work: (work.0.max(1), work.1.max(1)),
            drag: None,
            merge_selection: Vec::new(),
            merge_at: None,
            last_rect: None,
        }
    }

    pub fn effective_spacing(&self) -> i32 {
        if self.show_spacing {
            self.spacing
        } else {
            0
        }
    }

    pub fn min_size(&self) -> (i32, i32) {
        min_zone_size(self.work, self.effective_spacing())
    }

    /// The layout in FancyZones' format.
    pub fn to_info(&self) -> GridLayoutInfo {
        let mut info = self.model.to_info();
        info.show_spacing = self.show_spacing;
        info.spacing = self.spacing;
        info.sensitivity_radius = self.sensitivity_radius;
        info
    }

    pub fn cancel_merge(&mut self) {
        self.merge_selection.clear();
        self.merge_at = None;
    }

    pub fn merge_selected(&mut self) {
        let sel = std::mem::take(&mut self.merge_selection);
        self.model.merge(&sel);
        self.merge_at = None;
    }

    /// Draw the editor filling `rect` and handle input.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect) {
        self.last_rect = Some(rect);
        let resp = ui.interact(rect, ui.scope_id().with("grid-editor"), Sense::click_and_drag());
        let to_grid = |p: Pos2| -> (i32, i32) {
            (
                (((p.x - rect.left()) / rect.width()) * MULTIPLIER as f32).round() as i32,
                (((p.y - rect.top()) / rect.height()) * MULTIPLIER as f32).round() as i32,
            )
        };
        let to_screen = |x: i32, y: i32| -> Pos2 {
            Pos2::new(
                rect.left() + x as f32 / MULTIPLIER as f32 * rect.width(),
                rect.top() + y as f32 / MULTIPLIER as f32 * rect.height(),
            )
        };
        let shift = ui.input(|i| i.modifiers.shift);
        let min = self.min_size();
        let resizers = self.model.resizers();
        let pointer = resp.hover_pos().or_else(|| if resp.dragged() { resp.interact_pointer_pos() } else { None });

        // Which divider is under the pointer (within a few points).
        let grab = 6.0;
        let resizer_at = |p: Pos2| -> Option<usize> {
            resizers.iter().position(|r| {
                let a = to_screen(if r.orientation == Orientation::Vertical { r.pos } else { r.start }, if r.orientation == Orientation::Vertical { r.start } else { r.pos });
                let b = to_screen(if r.orientation == Orientation::Vertical { r.pos } else { r.end }, if r.orientation == Orientation::Vertical { r.end } else { r.pos });
                match r.orientation {
                    Orientation::Vertical => (p.x - a.x).abs() <= grab && p.y >= a.y && p.y <= b.y,
                    Orientation::Horizontal => (p.y - a.y).abs() <= grab && p.x >= a.x && p.x <= b.x,
                }
            })
        };

        // --- input
        if resp.drag_started() {
            // Where the button went down (the pointer has moved since).
            if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
                self.cancel_merge();
                if let Some(i) = resizer_at(p) {
                    self.drag = Some(Drag::Resizer { base: self.model.clone(), resizer: resizers[i].clone(), start: p });
                } else {
                    let (x, y) = to_grid(p);
                    if let Some(z) = self.model.zone_at(x, y) {
                        self.drag = Some(Drag::Press { zone: z, start: p, merging: false });
                    }
                }
            }
        }
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                match &mut self.drag {
                    Some(Drag::Resizer { base, resizer, start }) => {
                        let (dx, dy) = (to_grid(p).0 - to_grid(*start).0, to_grid(p).1 - to_grid(*start).1);
                        let delta = if resizer.orientation == Orientation::Vertical { dx } else { dy };
                        // Snap the divider to the other dividers' positions.
                        let (kx, ky) = base.key_points();
                        let keys: Vec<i32> = if resizer.orientation == Orientation::Vertical { kx } else { ky };
                        let keys: Vec<i32> = keys.into_iter().filter(|&k| k != resizer.pos).collect();
                        let (lo, hi) = base.drag_range(resizer, min);
                        let target = magnetic_snap(&keys, resizer.pos + delta, resizer.pos + lo, resizer.pos + hi);
                        let mut m = base.clone();
                        m.drag(resizer, target - resizer.pos, min);
                        self.model = m;
                    }
                    Some(Drag::Press { zone, start, merging }) if *merging || p.distance(*start) > 4.0 => {
                        *merging = true;
                        let (x, y) = to_grid(p);
                        if let Some(other) = self.model.zone_at(x, y) {
                            self.merge_selection = self.model.merge_closure(&[*zone, other]).0;
                        }
                    }
                    _ => {}
                }
            }
        }
        if resp.drag_stopped() {
            if let Some(Drag::Press { merging: true, .. }) = &self.drag {
                if self.merge_selection.len() > 1 {
                    self.merge_at = resp.interact_pointer_pos().or(pointer);
                } else {
                    self.cancel_merge();
                }
            }
            self.drag = None;
        }
        let split_at = |m: &GridModel, p: Pos2| -> Option<(usize, i32, Orientation)> {
            let (x, y) = to_grid(p);
            let z = m.zone_at(x, y)?;
            let zone = m.zones()[z];
            let (kx, ky) = m.key_points();
            let o = if shift { Orientation::Vertical } else { Orientation::Horizontal };
            let pos = match o {
                Orientation::Horizontal => magnetic_snap(&ky, y, zone.top, zone.bottom),
                Orientation::Vertical => magnetic_snap(&kx, x, zone.left, zone.right),
            };
            Some((z, pos, o))
        };
        if resp.clicked() && self.merge_at.is_none() {
            if let Some(p) = resp.interact_pointer_pos() {
                if resizer_at(p).is_none() {
                    if let Some((z, pos, o)) = split_at(&self.model, p) {
                        self.model.split(z, pos, o, min);
                    }
                }
            }
        } else if resp.clicked() {
            self.cancel_merge();
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_merge();
        }

        // --- drawing
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals().clone();
        painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
        let sx = rect.width() / self.work.0 as f32;
        let sy = rect.height() / self.work.1 as f32;
        let info = self.to_info();
        let px_rects = calculate_grid_zones(self.work.0, self.work.1, &info, self.effective_spacing()).ok();
        let fill = Color32::from_rgba_unmultiplied(0x2B, 0x88, 0xD8, 70);
        let sel_fill = Color32::from_rgba_unmultiplied(0x2B, 0x88, 0xD8, 150);
        for (i, z) in self.model.zones().iter().enumerate() {
            // Draw with the real spacing (as FancyZones will place windows).
            let r = match px_rects.as_ref().and_then(|v| v.get(i)) {
                Some(pr) => Rect::from_min_max(
                    rect.min + egui::vec2(pr.left as f32 * sx, pr.top as f32 * sy),
                    rect.min + egui::vec2(pr.right as f32 * sx, pr.bottom as f32 * sy),
                ),
                None => Rect::from_min_max(to_screen(z.left, z.top), to_screen(z.right, z.bottom)).shrink(2.0),
            };
            let selected = self.merge_selection.contains(&i);
            painter.rect(r, 4.0, if selected { sel_fill } else { fill }, Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
            painter.text(r.center(), egui::Align2::CENTER_CENTER, (i + 1).to_string(), egui::FontId::proportional(28.0), visuals.text_color());
            let w = (z.width() as i64 * self.work.0 as i64 / MULTIPLIER as i64) as i32;
            let h = (z.height() as i64 * self.work.1 as i64 / MULTIPLIER as i64) as i32;
            painter.text(
                r.center() + egui::vec2(0.0, 24.0),
                egui::Align2::CENTER_CENTER,
                format!("{w} × {h}"),
                egui::FontId::proportional(12.0),
                visuals.weak_text_color(),
            );
        }
        // Dividers.
        let hovered_resizer = match &self.drag {
            Some(Drag::Resizer { .. }) => None,
            _ => pointer.and_then(resizer_at),
        };
        for (i, r) in self.model.resizers().iter().enumerate() {
            let (a, b) = match r.orientation {
                Orientation::Vertical => (to_screen(r.pos, r.start), to_screen(r.pos, r.end)),
                Orientation::Horizontal => (to_screen(r.start, r.pos), to_screen(r.end, r.pos)),
            };
            let mid = a.lerp(b, 0.5);
            let hot = hovered_resizer == Some(i);
            let (hw, hh) = match r.orientation {
                Orientation::Vertical => (4.0, 22.0),
                Orientation::Horizontal => (22.0, 4.0),
            };
            let handle = Rect::from_center_size(mid, egui::vec2(hw * 2.0, hh * 2.0));
            painter.rect_filled(handle, 3.0, if hot { ACCENT } else { visuals.widgets.inactive.fg_stroke.color });
        }
        // Cursor + split preview.
        match &self.drag {
            Some(Drag::Resizer { resizer, .. }) => ui.ctx().set_cursor_icon(match resizer.orientation {
                Orientation::Vertical => CursorIcon::ResizeHorizontal,
                Orientation::Horizontal => CursorIcon::ResizeVertical,
            }),
            Some(Drag::Press { merging: true, .. }) => ui.ctx().set_cursor_icon(CursorIcon::Crosshair),
            _ => {
                if let Some(p) = pointer.filter(|p| rect.contains(*p)) {
                    if let Some(i) = hovered_resizer {
                        ui.ctx().set_cursor_icon(match resizers[i].orientation {
                            Orientation::Vertical => CursorIcon::ResizeHorizontal,
                            Orientation::Horizontal => CursorIcon::ResizeVertical,
                        });
                    } else if self.merge_at.is_none() {
                        if let Some((z, pos, o)) = split_at(&self.model, p) {
                            let zone = self.model.zones()[z];
                            let ok = self.model.can_split(z, pos, o, min);
                            let color = if ok { ACCENT } else { visuals.weak_text_color() };
                            let (a, b) = match o {
                                Orientation::Horizontal => (to_screen(zone.left, pos), to_screen(zone.right, pos)),
                                Orientation::Vertical => (to_screen(pos, zone.top), to_screen(pos, zone.bottom)),
                            };
                            painter.line_segment([a, b], Stroke::new(2.0, color));
                        }
                    }
                }
            }
        }
        // Merge button.
        if let Some(at) = self.merge_at {
            let mut merge = false;
            let mut cancel = false;
            egui::Area::new(ui.scope_id().with("grid-merge"))
                .order(egui::Order::Foreground)
                .fixed_pos(at + egui::vec2(8.0, 8.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            merge = ui.button(format!("Merge {} zones", self.merge_selection.len())).clicked();
                            cancel = ui.button("Cancel").clicked();
                        });
                    });
                });
            if merge {
                self.merge_selected();
            } else if cancel {
                self.cancel_merge();
            }
        }
        if resp.hovered() {
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fancyzones::layout::{grid_info, priority_grid_template, CustomLayout, CustomLayoutInfo};
    use fancyzones::{AppliedLayout, Layout, Rect as FzRect};

    const NO_MIN: (i32, i32) = (1, 1);

    fn info(rows: &[i32], cols: &[i32], map: &[&[i32]]) -> GridLayoutInfo {
        GridLayoutInfo::new(rows.to_vec(), cols.to_vec(), map.iter().map(|r| r.to_vec()).collect())
    }

    /// Zone rects FancyZones computes for this info on a work area.
    fn fz_rects(i: &GridLayoutInfo, w: i32, h: i32) -> Vec<FzRect> {
        calculate_grid_zones(w, h, i, 0).unwrap()
    }

    #[test]
    fn templates_round_trip_exactly() {
        for n in 1..=11 {
            let t = priority_grid_template(n).unwrap();
            let m = GridModel::from_info(&t).unwrap();
            m.check().unwrap();
            assert_eq!(m.len(), n);
            let back = m.to_info();
            assert_eq!(back.rows_percents, t.rows_percents, "n={n}");
            assert_eq!(back.columns_percents, t.columns_percents, "n={n}");
            assert_eq!(back.cell_child_map, t.cell_child_map, "n={n}");
            assert_eq!(fz_rects(&back, 1920, 1080), fz_rects(&t, 1920, 1080));
        }
        for n in 1..=20 {
            let t = grid_info(n).unwrap();
            let m = GridModel::from_info(&t).unwrap();
            assert_eq!(m.to_info().cell_child_map, t.cell_child_map, "n={n}");
            assert_eq!(fz_rects(&m.to_info(), 2560, 1440), fz_rects(&t, 2560, 1440));
        }
    }

    #[test]
    fn from_info_rejects_bad_maps() {
        // Not a rectangle (L shape).
        assert!(GridModel::from_info(&info(&[5000, 5000], &[5000, 5000], &[&[0, 0], &[0, 1]])).is_err());
        // Non-contiguous ids.
        assert!(GridModel::from_info(&info(&[10000], &[5000, 5000], &[&[0, 2]])).is_err());
        // Wrong sizes.
        assert!(GridModel::from_info(&info(&[10000], &[10000], &[&[0, 1]])).is_err());
        assert!(GridModel::from_info(&info(&[5000, 5000], &[10000], &[&[0]])).is_err());
        // Zero percent.
        assert!(GridModel::from_info(&info(&[10000], &[0, 10000], &[&[0, 1]])).is_err());
        assert!(GridModel::from_info(&info(&[10000], &[10000], &[&[-1]])).is_err());
    }

    #[test]
    fn from_info_normalizes_percent_sums() {
        let m = GridModel::from_info(&info(&[1], &[1, 1, 2], &[&[0, 1, 2]])).unwrap();
        let i = m.to_info();
        assert_eq!(i.rows_percents, vec![10000]);
        assert_eq!(i.columns_percents, vec![2500, 2500, 5000]);
    }

    #[test]
    fn new_layout_is_three_columns() {
        let m = GridModel::columns(3);
        let i = m.to_info();
        assert_eq!(i.rows, 1);
        assert_eq!(i.columns_percents, vec![3333, 3333, 3334]);
        assert_eq!(i.cell_child_map, vec![vec![0, 1, 2]]);
        assert_eq!(GridModel::single().to_info().cell_child_map, vec![vec![0]]);
    }

    #[test]
    fn split_horizontal_and_vertical() {
        let mut m = GridModel::columns(2);
        assert!(m.split(0, 4000, Orientation::Horizontal, NO_MIN));
        m.check().unwrap();
        let i = m.to_info();
        assert_eq!(i.rows_percents, vec![4000, 6000]);
        assert_eq!(i.columns_percents, vec![5000, 5000]);
        // New zones take ids 0 and 1; the old zone 1 becomes 2.
        assert_eq!(i.cell_child_map, vec![vec![0, 2], vec![1, 2]]);
        assert_eq!(i.zone_count(), 3);
        let r = fz_rects(&i, 1000, 1000);
        assert_eq!(r[0], FzRect::new(0, 0, 500, 400));
        assert_eq!(r[1], FzRect::new(0, 400, 500, 1000));
        assert_eq!(r[2], FzRect::new(500, 0, 1000, 1000));

        assert!(m.split(2, 7500, Orientation::Vertical, NO_MIN));
        let i = m.to_info();
        assert_eq!(i.columns_percents, vec![5000, 2500, 2500]);
        assert_eq!(i.cell_child_map, vec![vec![0, 2, 3], vec![1, 2, 3]]);
        m.check().unwrap();
    }

    #[test]
    fn split_respects_min_size_and_bounds() {
        let mut m = GridModel::single();
        let min = (1000, 1000);
        assert!(!m.can_split(0, 500, Orientation::Horizontal, min));
        assert!(!m.can_split(0, 9500, Orientation::Vertical, min));
        assert!(m.can_split(0, 1000, Orientation::Vertical, min));
        assert!(!m.split(0, 500, Orientation::Horizontal, min));
        assert_eq!(m.len(), 1);
        assert!(!m.split(5, 5000, Orientation::Horizontal, min));
        // Splitting exactly at the edge is never allowed (min >= 1).
        assert!(!m.split(0, 0, Orientation::Horizontal, NO_MIN));
        assert!(!m.split(0, MULTIPLIER, Orientation::Vertical, NO_MIN));
    }

    #[test]
    fn min_zone_size_from_pixels() {
        // 100 px + 2*16 on 1920 x 1080
        assert_eq!(min_zone_size((1920, 1080), 16), (687, 1222));
        assert_eq!(min_zone_size((1000, 1000), 0), (1000, 1000));
        assert_eq!(min_zone_size((10, 10), 0), (5000, 5000));
        assert_eq!(min_zone_size((1000, 1000), -10), (1000, 1000));
    }

    #[test]
    fn resizers_of_priority_grid() {
        let m = GridModel::from_info(&priority_grid_template(4).unwrap()).unwrap();
        // map [[0,1,2],[0,1,3]]: one horizontal resizer between 2 and 3, two vertical.
        let r = m.resizers();
        assert_eq!(r.len(), 3);
        let h: Vec<_> = r.iter().filter(|r| r.orientation == Orientation::Horizontal).collect();
        assert_eq!(h.len(), 1);
        assert_eq!((h[0].negative.clone(), h[0].positive.clone()), (vec![2], vec![3]));
        assert_eq!((h[0].pos, h[0].start, h[0].end), (5000, 7500, 10000));
        let v: Vec<_> = r.iter().filter(|r| r.orientation == Orientation::Vertical).collect();
        assert_eq!(v[0].negative, vec![0]);
        assert_eq!(v[0].positive, vec![1]);
        assert_eq!((v[0].pos, v[0].start, v[0].end), (2500, 0, 10000));
        assert_eq!(v[1].negative, vec![1]);
        assert_eq!(v[1].positive, vec![2, 3]);
    }

    #[test]
    fn drag_keeps_sum_and_clamps_to_min() {
        let mut m = GridModel::columns(3);
        let r = m.resizers().into_iter().find(|r| r.pos == 3333).unwrap();
        assert_eq!(m.drag(&r, 1000, NO_MIN), 1000);
        let i = m.to_info();
        assert_eq!(i.columns_percents, vec![4333, 2333, 3334]);
        assert_eq!(i.columns_percents.iter().sum::<i32>(), MULTIPLIER);
        // Clamped so the middle zone keeps min width 500.
        let r = m.resizers().into_iter().find(|r| r.pos == 4333).unwrap();
        assert_eq!(m.drag_range(&r, (500, 500)), (-3833, 1833));
        assert_eq!(m.drag(&r, 5000, (500, 500)), 1833);
        assert_eq!(m.to_info().columns_percents, vec![6166, 500, 3334]);
        m.check().unwrap();
        // A zone already below the minimum can't shrink further.
        let r = m.resizers().into_iter().find(|r| r.pos == 6166).unwrap();
        assert_eq!(m.drag(&r, 100, (1000, 1000)), 0);
    }

    #[test]
    fn drag_partial_divider_moves_only_its_zones() {
        // [[0,1],[2,1]] -> horizontal resizer between 0 and 2 only.
        let mut m = GridModel::from_info(&info(&[5000, 5000], &[5000, 5000], &[&[0, 1], &[2, 1]])).unwrap();
        let r = m.resizers().into_iter().find(|r| r.orientation == Orientation::Horizontal).unwrap();
        assert_eq!((r.start, r.end), (0, 5000));
        m.drag(&r, -2000, NO_MIN);
        m.check().unwrap();
        let i = m.to_info();
        assert_eq!(i.rows_percents, vec![3000, 7000]);
        assert_eq!(i.cell_child_map, vec![vec![0, 1], vec![2, 1]]);
        let rects = fz_rects(&i, 1000, 1000);
        assert_eq!(rects[1], FzRect::new(500, 0, 1000, 1000));
        assert_eq!(rects[2], FzRect::new(0, 300, 500, 1000));
    }

    #[test]
    fn drag_creates_and_collapses_columns() {
        // Two rows, each split vertically at the same x. Dragging only the top
        // row's divider makes 3 columns; dragging it back collapses to 2.
        let m = GridModel::from_info(&info(&[5000, 5000], &[5000, 5000], &[&[0, 1], &[2, 3]])).unwrap();
        let r = m.resizers().into_iter().filter(|r| r.orientation == Orientation::Vertical).collect::<Vec<_>>();
        assert_eq!(r.len(), 1, "both rows share one divider run");
        // Split the run apart: drag a divider of a model where they're separate.
        let mut m2 = GridModel::from_zones(vec![
            Zone::new(0, 0, 4000, 5000),
            Zone::new(4000, 0, 10000, 5000),
            Zone::new(0, 5000, 5000, 10000),
            Zone::new(5000, 5000, 10000, 10000),
        ])
        .unwrap();
        assert_eq!(m2.to_info().columns_percents, vec![4000, 1000, 5000]);
        assert_eq!(m2.to_info().cell_child_map, vec![vec![0, 1, 1], vec![2, 2, 3]]);
        let top = m2.resizers().into_iter().find(|r| r.orientation == Orientation::Vertical && r.start == 0).unwrap();
        m2.drag(&top, 1000, NO_MIN);
        assert_eq!(m2.to_info().columns_percents, vec![5000, 5000]);
        assert_eq!(m2.to_info().cell_child_map, vec![vec![0, 1], vec![2, 3]]);
        m.check().unwrap();
        m2.check().unwrap();
    }

    #[test]
    fn merge_closure_and_rectangles() {
        // 2x2 grid 0 1 / 2 3
        let m = GridModel::from_info(&grid_info(4).unwrap()).unwrap();
        assert!(m.is_rectangle(&[0, 1]));
        assert!(m.is_rectangle(&[0, 2]));
        assert!(!m.is_rectangle(&[0, 3]));
        assert!(m.is_rectangle(&[0, 1, 2, 3]));
        assert!(!m.is_rectangle(&[]));
        let (c, r) = m.merge_closure(&[0, 3]);
        assert_eq!(c, vec![0, 1, 2, 3]);
        assert_eq!(r, Zone::new(0, 0, MULTIPLIER, MULTIPLIER));
        // Priority grid 4: [[0,1,2],[0,1,3]] — merging 1 and 3 must pull in 2.
        let m = GridModel::from_info(&priority_grid_template(4).unwrap()).unwrap();
        assert_eq!(m.merge_closure(&[1, 3]).0, vec![1, 2, 3]);
        assert_eq!(m.merge_closure(&[2, 3]).0, vec![2, 3]);
        assert_eq!(m.merge_closure(&[9]).0, Vec::<usize>::new());
    }

    #[test]
    fn merge_produces_fancyzones_map() {
        let mut m = GridModel::from_info(&grid_info(4).unwrap()).unwrap();
        assert!(m.merge(&[1, 3]));
        m.check().unwrap();
        let i = m.to_info();
        assert_eq!(i.cell_child_map, vec![vec![0, 1], vec![2, 1]]);
        assert_eq!(i.zone_count(), 3);
        // Merge the remaining left column: everything collapses to 2 columns.
        assert!(m.merge(&[0, 2]));
        let i = m.to_info();
        assert_eq!(i.rows, 1);
        assert_eq!(i.cell_child_map, vec![vec![0, 1]]);
        assert!(!m.merge(&[0]));
        assert!(m.merge(&[0, 1]));
        assert_eq!(m.to_info().cell_child_map, vec![vec![0]]);
        assert!(!m.merge(&[]));
    }

    #[test]
    fn merge_closure_includes_lower_ids() {
        // [[0,1],[2,2]] merging 1 and 2 needs 0 as well; the result is id 0.
        let mut m = GridModel::from_info(&info(&[5000, 5000], &[5000, 5000], &[&[0, 1], &[2, 2]])).unwrap();
        assert!(m.merge(&[1, 2]));
        assert_eq!(m.len(), 1);
        m.check().unwrap();
    }

    #[test]
    fn magnetic_snap_like_powertoys() {
        let keys = [5000];
        // Far away: unchanged.
        assert_eq!(magnetic_snap(&keys, 2000, 0, 10000), 2000);
        // Magnet size = min(5000, 5000, 833)/2 = 416, dead zone 208.
        assert_eq!(magnetic_snap(&keys, 5100, 0, 10000), 5000);
        assert_eq!(magnetic_snap(&keys, 4800, 0, 10000), 5000);
        // In the magnet but outside the dead zone: pulled, but continuous.
        assert_eq!(magnetic_snap(&keys, 5300, 0, 10000), 5300 - (5416 - 5300));
        assert_eq!(magnetic_snap(&keys, 4700, 0, 10000), 4700 + (4700 - 4584));
        assert_eq!(magnetic_snap(&keys, 5416, 0, 10000), 5416);
        // Clamped to bounds; key points outside (low, high) are ignored.
        assert_eq!(magnetic_snap(&keys, -50, 0, 10000), 0);
        assert_eq!(magnetic_snap(&keys, 5100, 5050, 9000), 5100);
        assert_eq!(magnetic_snap(&[], 12000, 0, 10000), 10000);
    }

    #[test]
    fn editor_round_trip_through_fancyzones_json() {
        // A FancyZones custom-layouts.json entry -> edit -> JSON -> zones.
        let json = r#"{
            "uuid": "{D9D4F04A-9F5E-4D3B-8E54-5C1E3B5E1B11}", "name": "Custom grid", "type": "grid",
            "info": { "rows": 2, "columns": 3, "rows-percentage": [5000, 5000],
                      "columns-percentage": [3333, 3333, 3334], "cell-child-map": [[0, 1, 2], [0, 3, 2]],
                      "show-spacing": true, "spacing": 10, "sensitivity-radius": 25 } }"#;
        let mut l: CustomLayout = serde_json::from_str(json).unwrap();
        let CustomLayoutInfo::Grid(g) = &l.info else { panic!() };
        let mut ed = GridEditor::new(g, (1920, 1080));
        assert_eq!((ed.spacing, ed.sensitivity_radius, ed.show_spacing), (10, 25, true));
        assert_eq!(ed.to_info(), *g, "unchanged edit is identical");
        // Merge zones 1 and 3 (the middle column), split zone 2 (right column).
        assert!(ed.model.merge(&[1, 3]));
        assert!(ed.model.split(2, 2500, Orientation::Horizontal, ed.min_size()));
        let info = ed.to_info();
        assert_eq!(info.cell_child_map, vec![vec![0, 1, 2], vec![0, 1, 3]]);
        assert_eq!(info.rows_percents, vec![2500, 7500]);
        assert_eq!(info.rows, 2);
        assert_eq!(info.columns, 3);
        l.info = CustomLayoutInfo::Grid(info);
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(v["type"], "grid");
        assert_eq!(v["info"]["rows-percentage"], serde_json::json!([2500, 7500]));
        assert_eq!(v["info"]["columns-percentage"], serde_json::json!([3333, 3333, 3334]));
        assert_eq!(v["info"]["cell-child-map"], serde_json::json!([[0, 1, 2], [0, 1, 3]]));
        assert_eq!(v["info"]["spacing"], 10);
        let back: CustomLayout = serde_json::from_value(v).unwrap();
        assert_eq!(back, l);
        let lay = Layout::new(&AppliedLayout::custom(&back).with_spacing(false, 0), FzRect::new(0, 0, 1920, 1080), std::slice::from_ref(&back)).unwrap();
        assert_eq!(lay.zones.len(), 4);
        assert_eq!(lay.zones[1], FzRect::new(639, 0, 1279, 1080));
        assert_eq!(lay.zones[2], FzRect::new(1279, 0, 1920, 270));
        assert_eq!(lay.zones[3], FzRect::new(1279, 270, 1920, 1080));
    }

    #[test]
    fn random_operations_keep_invariants() {
        // Deterministic pseudo-random sequence of split/drag/merge.
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n.max(1)) as i32
        };
        let min = (300, 300);
        let mut m = GridModel::columns(3);
        for step in 0..2000 {
            match rnd(3) {
                0 => {
                    let z = rnd(m.len() as u64) as usize;
                    let o = if rnd(2) == 0 { Orientation::Horizontal } else { Orientation::Vertical };
                    m.split(z, rnd(MULTIPLIER as u64), o, min);
                }
                1 => {
                    let rs = m.resizers();
                    if !rs.is_empty() {
                        let r = &rs[rnd(rs.len() as u64) as usize];
                        m.drag(r, rnd(4000) - 2000, min);
                    }
                }
                _ => {
                    if m.len() > 6 || rnd(4) == 0 {
                        let a = rnd(m.len() as u64) as usize;
                        let b = rnd(m.len() as u64) as usize;
                        m.merge(&[a, b]);
                    }
                }
            }
            m.check().unwrap_or_else(|e| panic!("step {step}: {e}"));
            let info = m.to_info();
            assert_eq!(info.rows_percents.iter().sum::<i32>(), MULTIPLIER);
            assert_eq!(info.columns_percents.iter().sum::<i32>(), MULTIPLIER);
            info.validate().unwrap();
            assert_eq!(info.zone_count(), m.len());
            // FancyZones computes exactly these zones, in id order.
            let rects = calculate_grid_zones(10000, 10000, &info, 0).unwrap();
            let zs: Vec<FzRect> = m.zones().iter().map(|z| FzRect::new(z.left, z.top, z.right, z.bottom)).collect();
            assert_eq!(rects, zs, "step {step}");
            // ...and reading the JSON form back gives the same model.
            assert_eq!(GridModel::from_info(&info).unwrap(), m, "step {step}");
        }
    }
}
