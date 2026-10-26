//! Layout model and zone computation — a port of FancyZones'
//! `LayoutConfigurator.cpp`, `Layout.cpp` and `Zone.cpp`.
//!
//! Zone rectangles are first computed *relative to the work area* (exactly like
//! FancyZones) and then translated into global coordinates by [`Layout::new`].

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::geometry::{Point, Rect};

/// Fixed-point multiplier used by grid percentages (10000 == 100%).
pub const C_MULTIPLIER: i64 = 10000;
/// `ZoneConstants::MAX_NEGATIVE_SPACING`.
pub const MAX_NEGATIVE_SPACING: i32 = -20;
/// FancyZones uses an all-zero GUID for the built-in templates.
pub const NULL_GUID: &str = "{00000000-0000-0000-0000-000000000000}";

/// `DefaultValues` from `LayoutDefaults.h`.
pub mod defaults {
    pub const ZONE_COUNT: i32 = 3;
    pub const SHOW_SPACING: bool = true;
    pub const SPACING: i32 = 16;
    pub const SENSITIVITY_RADIUS: i32 = 20;
}

/// `FancyZonesDataTypes::ZoneSetLayoutType` with the same JSON spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutType {
    Blank,
    Focus,
    Columns,
    Rows,
    Grid,
    PriorityGrid,
    Custom,
}

impl fmt::Display for LayoutType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            LayoutType::Blank => "blank",
            LayoutType::Focus => "focus",
            LayoutType::Columns => "columns",
            LayoutType::Rows => "rows",
            LayoutType::Grid => "grid",
            LayoutType::PriorityGrid => "priority-grid",
            LayoutType::Custom => "custom",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for LayoutType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "blank" => LayoutType::Blank,
            "focus" => LayoutType::Focus,
            "columns" => LayoutType::Columns,
            "rows" => LayoutType::Rows,
            "grid" => LayoutType::Grid,
            "priority-grid" | "prioritygrid" | "priority_grid" => LayoutType::PriorityGrid,
            "custom" => LayoutType::Custom,
            other => return Err(format!("unknown layout type '{other}'")),
        })
    }
}

fn d_true() -> bool {
    true
}
fn d_spacing() -> i32 {
    defaults::SPACING
}
fn d_radius() -> i32 {
    defaults::SENSITIVITY_RADIUS
}
fn d_zone_count() -> i32 {
    defaults::ZONE_COUNT
}
fn d_null_guid() -> String {
    NULL_GUID.to_string()
}

/// The layout applied to a monitor (FancyZones `applied-layout` JSON object /
/// `LayoutData`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedLayout {
    /// For `custom` layouts, the uuid of the entry in the custom layouts list.
    #[serde(default = "d_null_guid")]
    pub uuid: String,
    #[serde(rename = "type")]
    pub kind: LayoutType,
    #[serde(rename = "show-spacing", default = "d_true")]
    pub show_spacing: bool,
    #[serde(default = "d_spacing")]
    pub spacing: i32,
    #[serde(rename = "zone-count", default = "d_zone_count")]
    pub zone_count: i32,
    #[serde(rename = "sensitivity-radius", default = "d_radius")]
    pub sensitivity_radius: i32,
}

impl Default for AppliedLayout {
    /// FancyZones' default for new monitors: priority grid with 3 zones.
    fn default() -> Self {
        Self::template(LayoutType::PriorityGrid, defaults::ZONE_COUNT)
    }
}

impl AppliedLayout {
    pub fn template(kind: LayoutType, zone_count: i32) -> Self {
        Self {
            uuid: NULL_GUID.to_string(),
            kind,
            show_spacing: defaults::SHOW_SPACING,
            spacing: defaults::SPACING,
            zone_count,
            sensitivity_radius: defaults::SENSITIVITY_RADIUS,
        }
    }

    /// An applied layout referring to a custom layout (copies its spacing,
    /// sensitivity and zone count like the FancyZones editor does).
    pub fn custom(layout: &CustomLayout) -> Self {
        let (show_spacing, spacing, radius) = match &layout.info {
            CustomLayoutInfo::Grid(g) => (g.show_spacing, g.spacing, g.sensitivity_radius),
            CustomLayoutInfo::Canvas(c) => (false, 0, c.sensitivity_radius),
        };
        Self {
            uuid: layout.uuid.clone(),
            kind: LayoutType::Custom,
            show_spacing,
            spacing,
            zone_count: layout.zone_count() as i32,
            sensitivity_radius: radius,
        }
    }

    pub fn with_spacing(mut self, show: bool, spacing: i32) -> Self {
        self.show_spacing = show;
        self.spacing = spacing;
        self
    }

    pub fn effective_spacing(&self) -> i32 {
        if self.show_spacing {
            self.spacing
        } else {
            0
        }
    }
}

/// `GridLayoutInfo` (also the `info` object of a custom `grid` layout).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridLayoutInfo {
    pub rows: usize,
    pub columns: usize,
    #[serde(rename = "rows-percentage")]
    pub rows_percents: Vec<i32>,
    #[serde(rename = "columns-percentage")]
    pub columns_percents: Vec<i32>,
    #[serde(rename = "cell-child-map")]
    pub cell_child_map: Vec<Vec<i32>>,
    #[serde(rename = "show-spacing", default = "d_true")]
    pub show_spacing: bool,
    #[serde(default = "d_spacing")]
    pub spacing: i32,
    #[serde(rename = "sensitivity-radius", default = "d_radius")]
    pub sensitivity_radius: i32,
}

impl GridLayoutInfo {
    pub fn new(rows_percents: Vec<i32>, columns_percents: Vec<i32>, cell_child_map: Vec<Vec<i32>>) -> Self {
        Self {
            rows: rows_percents.len(),
            columns: columns_percents.len(),
            rows_percents,
            columns_percents,
            cell_child_map,
            show_spacing: defaults::SHOW_SPACING,
            spacing: defaults::SPACING,
            sensitivity_radius: defaults::SENSITIVITY_RADIUS,
        }
    }

    /// Structural validation (mirrors the checks in `GridLayoutInfoJSON::FromJson`).
    pub fn validate(&self) -> Result<(), LayoutError> {
        if self.rows == 0 || self.columns == 0 {
            return Err(LayoutError::InvalidGrid("rows/columns must be > 0".into()));
        }
        if self.rows_percents.len() != self.rows
            || self.columns_percents.len() != self.columns
            || self.cell_child_map.len() != self.rows
        {
            return Err(LayoutError::InvalidGrid("percent/cell map sizes do not match rows/columns".into()));
        }
        if self.cell_child_map.iter().any(|r| r.len() != self.columns) {
            return Err(LayoutError::InvalidGrid("cell-child-map row has wrong column count".into()));
        }
        Ok(())
    }

    pub fn zone_count(&self) -> usize {
        let mut ids: Vec<i32> = self.cell_child_map.iter().flatten().copied().collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    }
}

/// One zone of a canvas layout, in reference (`ref-width` × `ref-height`) units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasZone {
    #[serde(rename = "X")]
    pub x: i32,
    #[serde(rename = "Y")]
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasLayoutInfo {
    #[serde(rename = "ref-width")]
    pub ref_width: i32,
    #[serde(rename = "ref-height")]
    pub ref_height: i32,
    pub zones: Vec<CanvasZone>,
    #[serde(rename = "sensitivity-radius", default = "d_radius")]
    pub sensitivity_radius: i32,
}

/// `type` + `info` of a FancyZones custom layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "info", rename_all = "lowercase")]
pub enum CustomLayoutInfo {
    Grid(GridLayoutInfo),
    Canvas(CanvasLayoutInfo),
}

/// One entry of FancyZones' `custom-layouts.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomLayout {
    pub uuid: String,
    pub name: String,
    #[serde(flatten)]
    pub info: CustomLayoutInfo,
}

impl CustomLayout {
    pub fn zone_count(&self) -> usize {
        match &self.info {
            CustomLayoutInfo::Grid(g) => g.zone_count(),
            CustomLayoutInfo::Canvas(c) => c.zones.len(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    InvalidWorkArea,
    InvalidZoneCount,
    InvalidGrid(String),
    InvalidZone,
    DuplicateZoneId(i32),
    CustomLayoutNotFound(String),
    ZoneCountMismatch { expected: i32, actual: usize },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::InvalidWorkArea => write!(f, "invalid work area"),
            LayoutError::InvalidZoneCount => write!(f, "invalid zone count"),
            LayoutError::InvalidGrid(s) => write!(f, "invalid grid layout: {s}"),
            LayoutError::InvalidZone => write!(f, "layout produced an invalid zone"),
            LayoutError::DuplicateZoneId(i) => write!(f, "duplicate zone id {i}"),
            LayoutError::CustomLayoutNotFound(u) => write!(f, "custom layout {u} not found"),
            LayoutError::ZoneCountMismatch { expected, actual } => {
                write!(f, "zone count mismatch: expected {expected}, got {actual}")
            }
        }
    }
}

impl std::error::Error for LayoutError {}

/// `Zone::isValid`.
fn zone_is_valid(r: &Rect) -> bool {
    r.left >= MAX_NEGATIVE_SPACING
        && r.right >= MAX_NEGATIVE_SPACING
        && r.top >= MAX_NEGATIVE_SPACING
        && r.bottom >= MAX_NEGATIVE_SPACING
        && r.width() >= 0
        && r.height() >= 0
}

fn push_zone(zones: &mut Vec<Rect>, r: Rect) -> Result<(), LayoutError> {
    if !zone_is_valid(&r) {
        return Err(LayoutError::InvalidZone);
    }
    zones.push(r);
    Ok(())
}

/// The PriorityGrid templates for 1..=11 zones (`predefinedPriorityGridLayouts`).
pub fn priority_grid_template(zone_count: usize) -> Option<GridLayoutInfo> {
    let g = |r: &[i32], c: &[i32], m: &[&[i32]]| {
        Some(GridLayoutInfo::new(r.to_vec(), c.to_vec(), m.iter().map(|x| x.to_vec()).collect()))
    };
    let r3 = [3333, 3334, 3333];
    let c3 = [2500, 5000, 2500];
    let c4 = [2500, 2500, 2500, 2500];
    match zone_count {
        1 => g(&[10000], &[10000], &[&[0]]),
        2 => g(&[10000], &[6667, 3333], &[&[0, 1]]),
        3 => g(&[10000], &c3, &[&[0, 1, 2]]),
        4 => g(&[5000, 5000], &c3, &[&[0, 1, 2], &[0, 1, 3]]),
        5 => g(&[5000, 5000], &c3, &[&[0, 1, 2], &[3, 1, 4]]),
        6 => g(&r3, &c3, &[&[0, 1, 2], &[0, 1, 3], &[4, 1, 5]]),
        7 => g(&r3, &c3, &[&[0, 1, 2], &[3, 1, 4], &[5, 1, 6]]),
        8 => g(&r3, &c4, &[&[0, 1, 2, 3], &[4, 1, 2, 5], &[6, 1, 2, 7]]),
        9 => g(&r3, &c4, &[&[0, 1, 2, 3], &[4, 1, 2, 5], &[6, 1, 7, 8]]),
        10 => g(&r3, &c4, &[&[0, 1, 2, 3], &[4, 1, 5, 6], &[7, 1, 8, 9]]),
        11 => g(&r3, &c4, &[&[0, 1, 2, 3], &[4, 1, 5, 6], &[7, 8, 9, 10]]),
        _ => None,
    }
}

/// `CalculateGridZones`. Returned rects are relative to the work area and
/// ordered by zone id.
pub fn calculate_grid_zones(width: i32, height: i32, info: &GridLayoutInfo, spacing: i32) -> Result<Vec<Rect>, LayoutError> {
    info.validate()?;
    let total_w = width as i64;
    let total_h = height as i64;

    struct Info {
        start: i64,
        end: i64,
    }
    let mut row_info = Vec::with_capacity(info.rows);
    let mut total = 0i64;
    for row in 0..info.rows {
        let start = total * total_h / C_MULTIPLIER;
        total += info.rows_percents[row] as i64;
        let end = total * total_h / C_MULTIPLIER;
        row_info.push(Info { start, end });
    }
    let mut col_info = Vec::with_capacity(info.columns);
    total = 0;
    for col in 0..info.columns {
        let start = total * total_w / C_MULTIPLIER;
        total += info.columns_percents[col] as i64;
        let end = total * total_w / C_MULTIPLIER;
        col_info.push(Info { start, end });
    }

    let map = &info.cell_child_map;
    let spacing = spacing as i64;
    let mut zones: BTreeMap<i32, Rect> = BTreeMap::new();
    for row in 0..info.rows {
        for col in 0..info.columns {
            let i = map[row][col];
            if (row == 0 || map[row - 1][col] != i) && (col == 0 || map[row][col - 1] != i) {
                let mut left = col_info[col].start;
                let mut top = row_info[row].start;
                let mut max_row = row;
                while max_row + 1 < info.rows && map[max_row + 1][col] == i {
                    max_row += 1;
                }
                let mut max_col = col;
                while max_col + 1 < info.columns && map[row][max_col + 1] == i {
                    max_col += 1;
                }
                let mut right = col_info[max_col].end;
                let mut bottom = row_info[max_row].end;

                top += if row == 0 { spacing } else { spacing / 2 };
                bottom -= if max_row == info.rows - 1 { spacing } else { spacing / 2 };
                left += if col == 0 { spacing } else { spacing / 2 };
                right -= if max_col == info.columns - 1 { spacing } else { spacing / 2 };

                let r = Rect::new(left as i32, top as i32, right as i32, bottom as i32);
                if i < 0 || !zone_is_valid(&r) {
                    return Err(LayoutError::InvalidZone);
                }
                if zones.insert(i, r).is_some() {
                    return Err(LayoutError::DuplicateZoneId(i));
                }
            }
        }
    }
    Ok(zones.into_values().collect())
}

/// `LayoutConfigurator::Focus`.
pub fn focus_zones(width: i32, height: i32, zone_count: i32) -> Result<Vec<Rect>, LayoutError> {
    let left = 100;
    let top = 100;
    let mut r = Rect::new(left, top, left + (width as f64 * 0.4) as i32, top + (height as f64 * 0.4) as i32);
    let inc = if zone_count <= 1 { 0 } else { 50 };
    let mut zones = Vec::new();
    for _ in 0..zone_count.max(0) {
        push_zone(&mut zones, r)?;
        r = r.offset(inc, inc);
    }
    Ok(zones)
}

/// `LayoutConfigurator::Rows`.
pub fn rows_zones(width: i32, height: i32, zone_count: i32, spacing: i32) -> Result<Vec<Rect>, LayoutError> {
    if zone_count <= 0 {
        return Ok(vec![]);
    }
    let (n, s) = (zone_count as i64, spacing as i64);
    let total_w = width as i64 - s * 2;
    let total_h = height as i64 - s * (n + 1);
    let mut top = s;
    let left = s;
    let mut zones = Vec::new();
    for i in 0..n {
        let right = total_w + s;
        let bottom = top + (i + 1) * total_h / n - i * total_h / n;
        push_zone(&mut zones, Rect::new(left as i32, top as i32, right as i32, bottom as i32))?;
        top = bottom + s;
    }
    Ok(zones)
}

/// `LayoutConfigurator::Columns`.
pub fn columns_zones(width: i32, height: i32, zone_count: i32, spacing: i32) -> Result<Vec<Rect>, LayoutError> {
    if zone_count <= 0 {
        return Ok(vec![]);
    }
    let (n, s) = (zone_count as i64, spacing as i64);
    let total_w = width as i64 - s * (n + 1);
    let total_h = height as i64 - s * 2;
    let top = s;
    let mut left = s;
    let mut zones = Vec::new();
    for i in 0..n {
        let right = left + (i + 1) * total_w / n - i * total_w / n;
        let bottom = total_h + s;
        push_zone(&mut zones, Rect::new(left as i32, top as i32, right as i32, bottom as i32))?;
        left = right + s;
    }
    Ok(zones)
}

/// The grid description used by `LayoutConfigurator::Grid` for `zone_count` zones.
pub fn grid_info(zone_count: i32) -> Option<GridLayoutInfo> {
    if zone_count <= 0 {
        return None;
    }
    let mut rows = 1;
    while zone_count / rows >= rows {
        rows += 1;
    }
    rows -= 1;
    let mut columns = zone_count / rows;
    if zone_count % rows != 0 {
        columns += 1;
    }
    let m = C_MULTIPLIER as i32;
    let rows_p = (0..rows).map(|r| m * (r + 1) / rows - m * r / rows).collect();
    let cols_p = (0..columns).map(|c| m * (c + 1) / columns - m * c / columns).collect();
    let mut map = vec![vec![0; columns as usize]; rows as usize];
    let mut index = 0;
    for row in map.iter_mut() {
        for cell in row.iter_mut() {
            *cell = index;
            index += 1;
            if index == zone_count {
                index -= 1;
            }
        }
    }
    Some(GridLayoutInfo::new(rows_p, cols_p, map))
}

/// `LayoutConfigurator::Grid`.
pub fn grid_zones(width: i32, height: i32, zone_count: i32, spacing: i32) -> Result<Vec<Rect>, LayoutError> {
    match grid_info(zone_count) {
        Some(info) => calculate_grid_zones(width, height, &info, spacing),
        None => Ok(vec![]),
    }
}

/// `LayoutConfigurator::PriorityGrid`. Note: like FancyZones, only 1..=10 use
/// the predefined table (`zoneCount < predefinedLayoutsCount`).
pub fn priority_grid_zones(width: i32, height: i32, zone_count: i32, spacing: i32) -> Result<Vec<Rect>, LayoutError> {
    if zone_count <= 0 {
        return Ok(vec![]);
    }
    if zone_count < 11 {
        let info = priority_grid_template(zone_count as usize).expect("template");
        return calculate_grid_zones(width, height, &info, spacing);
    }
    grid_zones(width, height, zone_count, spacing)
}

/// Canvas branch of `LayoutConfigurator::Custom` (DPI round-trip omitted; it
/// cancels out mathematically).
pub fn canvas_zones(width: i32, height: i32, info: &CanvasLayoutInfo) -> Result<Vec<Rect>, LayoutError> {
    if info.ref_width <= 0 || info.ref_height <= 0 {
        return Err(LayoutError::InvalidZone);
    }
    let w = width as f32;
    let h = height as f32;
    let mut zones = Vec::new();
    for z in &info.zones {
        let x = z.x as f32 * w / info.ref_width as f32;
        let y = z.y as f32 * h / info.ref_height as f32;
        let zw = z.width as f32 * w / info.ref_width as f32;
        let zh = z.height as f32 * h / info.ref_height as f32;
        push_zone(&mut zones, Rect::new(x as i32, y as i32, (x + zw) as i32, (y + zh) as i32))?;
    }
    Ok(zones)
}

/// Compute zone rects relative to a `width` × `height` work area
/// (the switch in `Layout::Init`).
pub fn compute_relative_zones(
    applied: &AppliedLayout,
    width: i32,
    height: i32,
    custom_layouts: &[CustomLayout],
) -> Result<Vec<Rect>, LayoutError> {
    if width <= 0 || height <= 0 {
        return Err(LayoutError::InvalidWorkArea);
    }
    let grid_type = matches!(
        applied.kind,
        LayoutType::Columns | LayoutType::Rows | LayoutType::Grid | LayoutType::PriorityGrid
    );
    if applied.zone_count < 0 || (applied.zone_count == 0 && grid_type) {
        return Err(LayoutError::InvalidZoneCount);
    }
    let spacing = applied.effective_spacing();
    let n = applied.zone_count;
    let zones = match applied.kind {
        LayoutType::Blank => vec![],
        LayoutType::Focus => focus_zones(width, height, n)?,
        LayoutType::Columns => columns_zones(width, height, n, spacing)?,
        LayoutType::Rows => rows_zones(width, height, n, spacing)?,
        LayoutType::Grid => grid_zones(width, height, n, spacing)?,
        LayoutType::PriorityGrid => priority_grid_zones(width, height, n, spacing)?,
        LayoutType::Custom => {
            let custom = find_custom(custom_layouts, &applied.uuid)
                .ok_or_else(|| LayoutError::CustomLayoutNotFound(applied.uuid.clone()))?;
            let zones = match &custom.info {
                CustomLayoutInfo::Canvas(c) => canvas_zones(width, height, c)?,
                CustomLayoutInfo::Grid(g) => calculate_grid_zones(width, height, g, spacing)?,
            };
            // FancyZones rejects a mismatch; the applied zone count is often stale
            // after editing so we only warn.
            if zones.len() as i32 != n {
                log::debug!("custom layout {}: zone-count {} but {} zones", custom.uuid, n, zones.len());
            }
            return Ok(zones);
        }
    };
    if applied.kind != LayoutType::Blank && zones.len() as i32 != n {
        return Err(LayoutError::ZoneCountMismatch { expected: n, actual: zones.len() });
    }
    Ok(zones)
}

/// GUID comparison ignoring case and braces.
pub fn guid_eq(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase();
    norm(a) == norm(b)
}

pub fn find_custom<'a>(custom: &'a [CustomLayout], uuid: &str) -> Option<&'a CustomLayout> {
    custom.iter().find(|c| guid_eq(&c.uuid, uuid))
}

/// `OverlappingZonesAlgorithm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlappingZonesAlgorithm {
    #[default]
    Smallest,
    Largest,
    Positional,
    ClosestCenter,
}

impl OverlappingZonesAlgorithm {
    /// FancyZones stores this setting as an int.
    pub fn from_index(i: i64) -> Self {
        match i {
            1 => Self::Largest,
            2 => Self::Positional,
            3 => Self::ClosestCenter,
            _ => Self::Smallest,
        }
    }
}

const OVERLAPPING_CENTERS_SENSITIVITY: i64 = 75;

/// A layout instantiated on a concrete work area. Zone rects are global.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub kind: LayoutType,
    pub uuid: String,
    pub work_area: Rect,
    pub sensitivity_radius: i32,
    /// Zones in global coordinates; index == FancyZones zone id.
    pub zones: Vec<Rect>,
}

impl Layout {
    pub fn new(applied: &AppliedLayout, work_area: Rect, custom_layouts: &[CustomLayout]) -> Result<Self, LayoutError> {
        let rel = compute_relative_zones(applied, work_area.width(), work_area.height(), custom_layouts)?;
        Ok(Self {
            kind: applied.kind,
            uuid: applied.uuid.clone(),
            work_area,
            sensitivity_radius: applied.sensitivity_radius,
            zones: rel.into_iter().map(|r| r.offset(work_area.left, work_area.top)).collect(),
        })
    }

    /// A layout with no zones (used when a layout fails to initialise).
    pub fn blank(work_area: Rect) -> Self {
        Self {
            kind: LayoutType::Blank,
            uuid: NULL_GUID.into(),
            work_area,
            sensitivity_radius: defaults::SENSITIVITY_RADIUS,
            zones: vec![],
        }
    }

    pub fn zone_count(&self) -> usize {
        self.zones.len()
    }

    /// `Layout::ZonesFromPoint`: zones under `pt`, choosing between overlapping
    /// zones with `algorithm`. Non-overlapping neighbours within the
    /// sensitivity radius are all returned (that is how dragging onto the
    /// border between two zones selects both).
    pub fn zones_from_point(&self, pt: Point, algorithm: OverlappingZonesAlgorithm) -> Vec<usize> {
        let sr = self.sensitivity_radius;
        let mut captured = Vec::new();
        let mut strictly = 0usize;
        for (id, z) in self.zones.iter().enumerate() {
            if z.left - sr <= pt.x && pt.x <= z.right + sr && z.top - sr <= pt.y && pt.y <= z.bottom + sr {
                captured.push(id);
            }
            if z.left <= pt.x && pt.x < z.right && z.top <= pt.y && pt.y < z.bottom {
                strictly += 1;
            }
        }
        if captured.len() == 1 && strictly == 0 {
            return vec![];
        }

        let mut overlap = false;
        'outer: for i in 0..captured.len() {
            for j in i + 1..captured.len() {
                let a = self.zones[captured[i]];
                let b = self.zones[captured[j]];
                if a.top.max(b.top) + sr < a.bottom.min(b.bottom) && a.left.max(b.left) + sr < a.right.min(b.right) {
                    overlap = true;
                    break 'outer;
                }
            }
        }
        if !overlap {
            return captured;
        }
        match algorithm {
            OverlappingZonesAlgorithm::Smallest => self.select_priority(&captured, |a, b| a.area() < b.area()),
            OverlappingZonesAlgorithm::Largest => self.select_priority(&captured, |a, b| a.area() > b.area()),
            OverlappingZonesAlgorithm::Positional => self.select_subregion(&captured, pt),
            OverlappingZonesAlgorithm::ClosestCenter => self.select_closest_center(&captured, pt),
        }
    }

    fn select_priority(&self, captured: &[usize], better: impl Fn(&Rect, &Rect) -> bool) -> Vec<usize> {
        let mut chosen = 0;
        for i in 1..captured.len() {
            if better(&self.zones[captured[i]], &self.zones[captured[chosen]]) {
                chosen = i;
            }
        }
        vec![captured[chosen]]
    }

    fn select_subregion(&self, captured: &[usize], pt: Point) -> Vec<usize> {
        let half = self.sensitivity_radius / 2;
        let expand = |r: Rect| Rect::new(r.left - half, r.top - half, r.right + half, r.bottom + half);
        let mut ov = expand(self.zones[captured[0]]);
        for &c in &captured[1..] {
            let cur = expand(self.zones[c]);
            ov.top = ov.top.max(cur.top);
            ov.left = ov.left.max(cur.left);
            ov.bottom = ov.bottom.min(cur.bottom);
            ov.right = ov.right.min(cur.right);
        }
        let width = (ov.right - ov.left).max(1) as i64;
        let height = (ov.bottom - ov.top).max(1) as i64;
        let n = captured.len() as i64;
        let idx = if height > width {
            (pt.y as i64 - ov.top as i64) * n / height
        } else {
            (pt.x as i64 - ov.left as i64) * n / width
        };
        vec![captured[idx.clamp(0, n - 1) as usize]]
    }

    fn select_closest_center(&self, captured: &[usize], pt: Point) -> Vec<usize> {
        let center = |r: &Rect| ((r.right as i64 + r.left as i64) / 2, (r.top as i64 + r.bottom as i64) / 2);
        let diff = |a: (i64, i64), b: (i64, i64)| (a.0 - b.0).pow(2) + (a.1 - b.1).pow(2);
        let p = (pt.x as i64, pt.y as i64);
        self.select_priority(captured, |a, b| {
            if diff(center(a), center(b)) > OVERLAPPING_CENTERS_SENSITIVITY {
                diff(center(a), p) < diff(center(b), p)
            } else {
                a.area() < b.area()
            }
        })
    }

    /// `Layout::GetCombinedZoneRange`: every zone fully inside the bounding
    /// box of `initial ∪ final` (used for spanning multiple zones).
    pub fn combined_zone_range(&self, initial: &[usize], fin: &[usize]) -> Vec<usize> {
        let mut combined: Vec<usize> = initial.iter().chain(fin.iter()).copied().collect();
        combined.sort_unstable();
        combined.dedup();
        let Some(bounds) = self.combined_zones_rect(&combined) else {
            return vec![];
        };
        self.zones
            .iter()
            .enumerate()
            .filter(|(_, r)| bounds.contains_rect(r))
            .map(|(i, _)| i)
            .collect()
    }

    /// `Layout::GetCombinedZonesRect`: bounding box of the given zones.
    pub fn combined_zones_rect(&self, ids: &[usize]) -> Option<Rect> {
        ids.iter()
            .filter_map(|&i| self.zones.get(i))
            .fold(None, |acc: Option<Rect>, r| Some(acc.map_or(*r, |a| a.union(r))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wa() -> Rect {
        Rect::new(0, 0, 1920, 1080)
    }

    fn applied(kind: LayoutType, n: i32, spacing: i32) -> AppliedLayout {
        AppliedLayout::template(kind, n).with_spacing(spacing != 0, spacing)
    }

    #[test]
    fn columns_no_spacing_sum_exact() {
        let z = columns_zones(1920, 1080, 3, 0).unwrap();
        assert_eq!(z, vec![Rect::new(0, 0, 640, 1080), Rect::new(640, 0, 1280, 1080), Rect::new(1280, 0, 1920, 1080)]);
        let z = columns_zones(1000, 500, 3, 0).unwrap();
        assert_eq!(z.iter().map(|r| r.width()).sum::<i32>(), 1000);
        assert_eq!(z[2].right, 1000);
    }

    #[test]
    fn columns_with_spacing() {
        // totalWidth = 1920 - 16*4 = 1856; widths 618,619,619
        let z = columns_zones(1920, 1080, 3, 16).unwrap();
        assert_eq!(z[0], Rect::new(16, 16, 16 + 618, 1064));
        assert_eq!(z[1].left, 16 + 618 + 16);
        assert_eq!(z[2].right, 1920 - 16);
        for r in &z {
            assert_eq!(r.top, 16);
            assert_eq!(r.bottom, 1064);
        }
    }

    #[test]
    fn rows_with_spacing() {
        let z = rows_zones(1920, 1080, 2, 10).unwrap();
        // totalHeight = 1080 - 30 = 1050 -> 525 each
        assert_eq!(z, vec![Rect::new(10, 10, 1910, 535), Rect::new(10, 545, 1910, 1070)]);
    }

    #[test]
    fn grid_shapes() {
        let i = grid_info(4).unwrap();
        assert_eq!((i.rows, i.columns), (2, 2));
        assert_eq!(i.cell_child_map, vec![vec![0, 1], vec![2, 3]]);
        let i = grid_info(5).unwrap();
        assert_eq!((i.rows, i.columns), (2, 3));
        // last cell repeats the last zone index (merged)
        assert_eq!(i.cell_child_map, vec![vec![0, 1, 2], vec![3, 4, 4]]);
        assert_eq!(i.columns_percents, vec![3333, 3333, 3334]);
        let i = grid_info(3).unwrap();
        assert_eq!((i.rows, i.columns), (1, 3));
        let i = grid_info(10).unwrap();
        assert_eq!((i.rows, i.columns), (3, 4));
        assert_eq!(i.cell_child_map[2], vec![8, 9, 9, 9]);
    }

    #[test]
    fn grid_zones_with_spacing() {
        let z = grid_zones(1920, 1080, 4, 16).unwrap();
        assert_eq!(z.len(), 4);
        assert_eq!(z[0], Rect::new(16, 16, 960 - 8, 540 - 8));
        assert_eq!(z[1], Rect::new(960 + 8, 16, 1920 - 16, 540 - 8));
        assert_eq!(z[2], Rect::new(16, 540 + 8, 960 - 8, 1080 - 16));
        assert_eq!(z[3], Rect::new(968, 548, 1904, 1064));
        let z = grid_zones(1920, 1080, 5, 0).unwrap();
        assert_eq!(z[4], Rect::new(639, 540, 1920, 1080));
    }

    #[test]
    fn priority_grid_matches_table() {
        let z = priority_grid_zones(1920, 1080, 3, 0).unwrap();
        assert_eq!(z, vec![Rect::new(0, 0, 480, 1080), Rect::new(480, 0, 1440, 1080), Rect::new(1440, 0, 1920, 1080)]);
        let z = priority_grid_zones(1920, 1080, 2, 0).unwrap();
        // 6667 * 1920 / 10000 = 1280 (1280.06)
        assert_eq!(z, vec![Rect::new(0, 0, 1280, 1080), Rect::new(1280, 0, 1920, 1080)]);
        let z = priority_grid_zones(1920, 1080, 4, 0).unwrap();
        assert_eq!(z[0], Rect::new(0, 0, 480, 1080));
        assert_eq!(z[2], Rect::new(1440, 0, 1920, 540));
        assert_eq!(z[3], Rect::new(1440, 540, 1920, 1080));
        for n in 1..=12 {
            assert_eq!(priority_grid_zones(1920, 1080, n, 16).unwrap().len(), n as usize, "n={n}");
        }
        // 11 falls through to plain grid like FancyZones
        assert_eq!(priority_grid_zones(1920, 1080, 11, 0).unwrap(), grid_zones(1920, 1080, 11, 0).unwrap());
    }

    #[test]
    fn focus_layout() {
        let z = focus_zones(1920, 1080, 3).unwrap();
        assert_eq!(z[0], Rect::new(100, 100, 100 + 768, 100 + 432));
        assert_eq!(z[2], Rect::new(200, 200, 968, 632));
        assert_eq!(focus_zones(1920, 1080, 1).unwrap().len(), 1);
    }

    #[test]
    fn canvas_scaling() {
        let info = CanvasLayoutInfo {
            ref_width: 1000,
            ref_height: 500,
            zones: vec![CanvasZone { x: 0, y: 0, width: 500, height: 500 }, CanvasZone { x: 500, y: 250, width: 500, height: 250 }],
            sensitivity_radius: 20,
        };
        let z = canvas_zones(2000, 1000, &info).unwrap();
        assert_eq!(z, vec![Rect::new(0, 0, 1000, 1000), Rect::new(1000, 500, 2000, 1000)]);
    }

    #[test]
    fn custom_grid_layout_and_offsets() {
        let mut g = GridLayoutInfo::new(vec![5000, 5000], vec![5000, 5000], vec![vec![0, 1], vec![0, 2]]);
        g.spacing = 0;
        let c = CustomLayout { uuid: "{ABC}".into(), name: "x".into(), info: CustomLayoutInfo::Grid(g) };
        let ap = AppliedLayout::custom(&c);
        assert_eq!(ap.zone_count, 3);
        let l = Layout::new(&ap, Rect::new(100, 50, 1100, 1050), std::slice::from_ref(&c)).unwrap();
        assert_eq!(l.zones[0], Rect::new(100, 50, 600, 1050));
        assert_eq!(l.zones[2], Rect::new(600, 550, 1100, 1050));
        let missing = AppliedLayout { uuid: "{nope}".into(), ..ap };
        assert!(matches!(Layout::new(&missing, wa(), &[c]), Err(LayoutError::CustomLayoutNotFound(_))));
    }

    #[test]
    fn invalid_inputs() {
        assert_eq!(compute_relative_zones(&applied(LayoutType::Grid, 0, 0), 100, 100, &[]), Err(LayoutError::InvalidZoneCount));
        assert_eq!(compute_relative_zones(&applied(LayoutType::Grid, 2, 0), 0, 100, &[]), Err(LayoutError::InvalidWorkArea));
        assert_eq!(compute_relative_zones(&applied(LayoutType::Blank, 0, 0), 100, 100, &[]), Ok(vec![]));
        // huge spacing makes zones invalid
        assert!(compute_relative_zones(&applied(LayoutType::Columns, 10, 200), 1000, 1000, &[]).is_err());
        let g = GridLayoutInfo::new(vec![5000, 5000], vec![10000], vec![vec![0]]);
        assert!(g.validate().is_err());
        // duplicate non-contiguous ids
        let g = GridLayoutInfo::new(vec![10000], vec![5000, 5000], vec![vec![0, 0]]);
        assert_eq!(calculate_grid_zones(100, 100, &g, 0).unwrap().len(), 1);
        let g = GridLayoutInfo::new(vec![5000, 5000], vec![5000, 5000], vec![vec![0, 1], vec![1, 0]]);
        assert!(matches!(calculate_grid_zones(100, 100, &g, 0), Err(LayoutError::DuplicateZoneId(_))));
    }

    #[test]
    fn hit_test_inside_and_border() {
        let l = Layout::new(&applied(LayoutType::Columns, 3, 0), wa(), &[]).unwrap();
        let alg = OverlappingZonesAlgorithm::Smallest;
        assert_eq!(l.zones_from_point(Point::new(100, 500), alg), vec![0]);
        assert_eq!(l.zones_from_point(Point::new(1000, 500), alg), vec![1]);
        // near the border between 0 and 1 -> both (span)
        assert_eq!(l.zones_from_point(Point::new(645, 500), alg), vec![0, 1]);
        assert_eq!(l.zones_from_point(Point::new(620, 500), alg), vec![0, 1]);
        assert_eq!(l.zones_from_point(Point::new(619, 500), alg), vec![0]);
        // outside work area but within radius of only one zone -> nothing
        assert_eq!(l.zones_from_point(Point::new(-5, 500), alg), Vec::<usize>::new());
    }

    #[test]
    fn hit_test_spacing_gap() {
        // spacing 16 -> gap between zones; a point in the gap is within radius of both
        let l = Layout::new(&applied(LayoutType::Columns, 2, 16), wa(), &[]).unwrap();
        let gap_x = l.zones[0].right + 8;
        assert_eq!(l.zones_from_point(Point::new(gap_x, 500), OverlappingZonesAlgorithm::Smallest), vec![0, 1]);
    }

    #[test]
    fn hit_test_overlapping_algorithms() {
        let info = CanvasLayoutInfo {
            ref_width: 1000,
            ref_height: 1000,
            zones: vec![CanvasZone { x: 0, y: 0, width: 1000, height: 1000 }, CanvasZone { x: 100, y: 100, width: 300, height: 300 }],
            sensitivity_radius: 20,
        };
        let c = CustomLayout { uuid: "{C}".into(), name: "c".into(), info: CustomLayoutInfo::Canvas(info) };
        let l = Layout::new(&AppliedLayout::custom(&c), Rect::new(0, 0, 1000, 1000), &[c]).unwrap();
        let p = Point::new(200, 200);
        assert_eq!(l.zones_from_point(p, OverlappingZonesAlgorithm::Smallest), vec![1]);
        assert_eq!(l.zones_from_point(p, OverlappingZonesAlgorithm::Largest), vec![0]);
        assert_eq!(l.zones_from_point(p, OverlappingZonesAlgorithm::ClosestCenter), vec![1]);
        // positional: overlap = small zone expanded by 10 -> 90..410 ; x=100 -> idx 0, x=400 -> idx 1
        assert_eq!(l.zones_from_point(Point::new(100, 200), OverlappingZonesAlgorithm::Positional), vec![0]);
        assert_eq!(l.zones_from_point(Point::new(400, 200), OverlappingZonesAlgorithm::Positional), vec![1]);
        // outside the small zone: only the big one strictly + captured
        assert_eq!(l.zones_from_point(Point::new(800, 800), OverlappingZonesAlgorithm::Smallest), vec![0]);
    }

    #[test]
    fn combined_range_spans() {
        // 2x2 grid: 0 1 / 2 3
        let l = Layout::new(&applied(LayoutType::Grid, 4, 0), wa(), &[]).unwrap();
        assert_eq!(l.combined_zone_range(&[0], &[3]), vec![0, 1, 2, 3]);
        assert_eq!(l.combined_zone_range(&[0], &[1]), vec![0, 1]);
        assert_eq!(l.combined_zone_range(&[2], &[2]), vec![2]);
        assert_eq!(l.combined_zone_range(&[], &[]), Vec::<usize>::new());
        assert_eq!(l.combined_zones_rect(&[0, 3]), Some(wa()));
        assert_eq!(l.combined_zones_rect(&[]), None);
        // priority grid 3: spanning 0 -> 2 includes the middle
        let l = Layout::new(&applied(LayoutType::PriorityGrid, 3, 0), wa(), &[]).unwrap();
        assert_eq!(l.combined_zone_range(&[0], &[2]), vec![0, 1, 2]);
    }

    #[test]
    fn layout_type_strings() {
        assert_eq!(serde_json::to_string(&LayoutType::PriorityGrid).unwrap(), "\"priority-grid\"");
        assert_eq!("priority-grid".parse::<LayoutType>().unwrap(), LayoutType::PriorityGrid);
        assert_eq!(LayoutType::Rows.to_string(), "rows");
        assert!(guid_eq("{AbC}", "abc"));
    }
}
