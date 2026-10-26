//! Image Resizer, ported from PowerToys: size presets (Small 854×480,
//! Medium 1366×768, Large 1920×1080, Phone 320×568, custom), fit modes
//! (fill, fit, stretch), units (px, %, cm, in), "only shrink", "ignore
//! orientation", replace originals or `%1 (%2)`-style names, output format,
//! JPEG quality and metadata handling.
//!
//! The size math ([`compute_dimensions`]) and file naming
//! ([`destination_path`]) follow PowerToys' `ResizeOperation.cs`. Images are
//! decoded and encoded with ImageIO / CoreGraphics on macOS (fast,
//! subsampled downscaling with `CGImageSourceCreateThumbnailAtIndex`) and
//! WIC on Windows.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
pub(crate) mod mac;
#[cfg(windows)]
mod win;

pub mod cli;

// ---------------------------------------------------------------------------
// Options

/// How the image is fitted into the target size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ResizeFit {
    /// Cover the box, cropping the overflow (centered).
    Fill,
    /// Fit inside the box, keeping the aspect ratio.
    #[default]
    Fit,
    /// Exactly the box, distorting if needed.
    Stretch,
}

/// The unit of a size's width and height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ResizeUnit {
    #[default]
    Pixel,
    Percent,
    Centimeter,
    Inch,
}

impl ResizeUnit {
    pub const ALL: [ResizeUnit; 4] = [
        ResizeUnit::Pixel,
        ResizeUnit::Percent,
        ResizeUnit::Centimeter,
        ResizeUnit::Inch,
    ];

    pub fn short(self) -> &'static str {
        match self {
            ResizeUnit::Pixel => "px",
            ResizeUnit::Percent => "%",
            ResizeUnit::Centimeter => "cm",
            ResizeUnit::Inch => "in",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ResizeUnit::Pixel => "Pixels",
            ResizeUnit::Percent => "Percent",
            ResizeUnit::Centimeter => "Centimeters",
            ResizeUnit::Inch => "Inches",
        }
    }

    /// Parse `px`, `%`, `percent`, `cm`, `in`, `inch` …
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "px" | "pixel" | "pixels" => ResizeUnit::Pixel,
            "%" | "pct" | "percent" => ResizeUnit::Percent,
            "cm" | "centimeter" | "centimeters" => ResizeUnit::Centimeter,
            "in" | "inch" | "inches" | "\"" => ResizeUnit::Inch,
            _ => return None,
        })
    }
}

impl ResizeFit {
    pub fn label(self) -> &'static str {
        match self {
            ResizeFit::Fill => "Fill",
            ResizeFit::Fit => "Fit",
            ResizeFit::Stretch => "Stretch",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "fill" | "crop" => ResizeFit::Fill,
            "fit" => ResizeFit::Fit,
            "stretch" => ResizeFit::Stretch,
            _ => return None,
        })
    }
}

/// A named target size. A width or height of 0 means "auto" (keep the
/// aspect ratio from the other one).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResizeSize {
    pub name: String,
    pub fit: ResizeFit,
    pub width: f64,
    pub height: f64,
    pub unit: ResizeUnit,
}

impl Default for ResizeSize {
    fn default() -> Self {
        Self::default_custom()
    }
}

impl ResizeSize {
    pub fn new(name: &str, fit: ResizeFit, width: f64, height: f64, unit: ResizeUnit) -> Self {
        Self {
            name: name.into(),
            fit,
            width,
            height,
            unit,
        }
    }

    /// PowerToys' default presets.
    pub fn default_presets() -> Vec<ResizeSize> {
        vec![
            Self::new("Small", ResizeFit::Fit, 854.0, 480.0, ResizeUnit::Pixel),
            Self::new("Medium", ResizeFit::Fit, 1366.0, 768.0, ResizeUnit::Pixel),
            Self::new("Large", ResizeFit::Fit, 1920.0, 1080.0, ResizeUnit::Pixel),
            Self::new("Phone", ResizeFit::Fit, 320.0, 568.0, ResizeUnit::Pixel),
        ]
    }

    /// PowerToys' default custom size (1024 × 640 px, fit).
    pub fn default_custom() -> Self {
        Self::new("Custom", ResizeFit::Fit, 1024.0, 640.0, ResizeUnit::Pixel)
    }

    /// Width or height is "auto".
    pub fn has_auto(&self) -> bool {
        self.width == 0.0 || self.height == 0.0 || self.width.is_nan() || self.height.is_nan()
    }

    /// "1366 × 768 px", "50 %", "auto × 600 px".
    pub fn describe(&self) -> String {
        let n = |v: f64| {
            if v == 0.0 || v.is_nan() {
                "auto".to_string()
            } else {
                fmt_num(v)
            }
        };
        if self.unit == ResizeUnit::Percent && self.fit != ResizeFit::Stretch {
            return format!("{} %", n(self.width));
        }
        format!(
            "{} × {} {}",
            n(self.width),
            n(self.height),
            self.unit.short()
        )
    }

    /// The target width in pixels for an image `original` px wide at `dpi`.
    pub fn pixel_width(&self, original: u32, dpi: f64) -> f64 {
        self.to_pixels(self.width, original, dpi)
    }

    /// The target height in pixels. With percent (and not stretch) the
    /// width's percentage applies to both sides.
    pub fn pixel_height(&self, original: u32, dpi: f64) -> f64 {
        let v = if self.fit != ResizeFit::Stretch && self.unit == ResizeUnit::Percent {
            self.width
        } else {
            self.height
        };
        self.to_pixels(v, original, dpi)
    }

    fn to_pixels(&self, value: f64, original: u32, dpi: f64) -> f64 {
        if value == 0.0 || value.is_nan() {
            return if self.fit == ResizeFit::Fit {
                f64::INFINITY
            } else {
                f64::from(original)
            };
        }
        let dpi = if dpi > 0.0 && dpi.is_finite() {
            dpi
        } else {
            72.0
        };
        match self.unit {
            ResizeUnit::Inch => value * dpi,
            ResizeUnit::Centimeter => value * dpi / 2.54,
            ResizeUnit::Percent => value / 100.0 * f64::from(original),
            ResizeUnit::Pixel => value,
        }
    }
}

/// `854` for 854.0, `12.5` for 12.5.
pub fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// The encoded format of the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum OutputFormat {
    /// The input's format (JPEG when it can't be written).
    #[default]
    Keep,
    Jpeg,
    Png,
    Heic,
    Webp,
    Tiff,
}

impl OutputFormat {
    pub const ALL: [OutputFormat; 6] = [
        OutputFormat::Keep,
        OutputFormat::Jpeg,
        OutputFormat::Png,
        OutputFormat::Heic,
        OutputFormat::Webp,
        OutputFormat::Tiff,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OutputFormat::Keep => "Same as original",
            OutputFormat::Jpeg => "JPEG",
            OutputFormat::Png => "PNG",
            OutputFormat::Heic => "HEIC",
            OutputFormat::Webp => "WebP",
            OutputFormat::Tiff => "TIFF",
        }
    }

    /// The file extension (without the dot) for a concrete format.
    pub fn extension(self) -> Option<&'static str> {
        Some(match self {
            OutputFormat::Keep => return None,
            OutputFormat::Jpeg => "jpg",
            OutputFormat::Png => "png",
            OutputFormat::Heic => "heic",
            OutputFormat::Webp => "webp",
            OutputFormat::Tiff => "tiff",
        })
    }

    /// The extensions files of this format may already have.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            OutputFormat::Keep => &[],
            OutputFormat::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
            OutputFormat::Png => &["png"],
            OutputFormat::Heic => &["heic", "heif", "hif"],
            OutputFormat::Webp => &["webp"],
            OutputFormat::Tiff => &["tif", "tiff"],
        }
    }

    /// The Uniform Type Identifier (macOS).
    pub fn uti(self) -> Option<&'static str> {
        Some(match self {
            OutputFormat::Keep => return None,
            OutputFormat::Jpeg => "public.jpeg",
            OutputFormat::Png => "public.png",
            OutputFormat::Heic => "public.heic",
            OutputFormat::Webp => "org.webmproject.webp",
            OutputFormat::Tiff => "public.tiff",
        })
    }

    /// The concrete format for a UTI.
    pub fn from_uti(uti: &str) -> Option<Self> {
        OutputFormat::ALL.into_iter().find(|f| f.uti() == Some(uti))
    }

    /// The concrete format for a file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        let e = ext.to_ascii_lowercase();
        OutputFormat::ALL
            .into_iter()
            .find(|f| f.extensions().contains(&e.as_str()))
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        if matches!(s.as_str(), "keep" | "same" | "original") {
            return Some(OutputFormat::Keep);
        }
        Self::from_extension(&s)
    }

    /// Lossy formats take a quality setting.
    pub fn is_lossy(self) -> bool {
        matches!(
            self,
            OutputFormat::Jpeg | OutputFormat::Heic | OutputFormat::Webp
        )
    }
}

/// The formats this machine can write ([`OutputFormat::Keep`] first).
pub fn available_formats() -> Vec<OutputFormat> {
    #[cfg(target_os = "macos")]
    {
        let enc = mac::encodable_utis();
        OutputFormat::ALL
            .into_iter()
            .filter(|f| f.uti().is_none_or(|u| enc.iter().any(|e| e == u)))
            .collect()
    }
    #[cfg(windows)]
    {
        vec![
            OutputFormat::Keep,
            OutputFormat::Jpeg,
            OutputFormat::Png,
            OutputFormat::Tiff,
        ]
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        vec![OutputFormat::Keep]
    }
}

/// Everything about one resize run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ResizeOptions {
    pub size: ResizeSize,
    /// Never enlarge (PowerToys "Make pictures smaller but not larger").
    pub shrink_only: bool,
    /// Swap the box's width and height to match portrait / landscape
    /// pictures (PowerToys "Ignore the orientation of pictures").
    pub ignore_orientation: bool,
    /// Overwrite the originals (moved to the Trash first on macOS) instead
    /// of writing new files next to them.
    pub replace: bool,
    /// The new file's name: `%1` original name, `%2` size name, `%3` / `%4`
    /// selected width / height, `%5` / `%6` actual width / height.
    pub file_name_format: String,
    pub format: OutputFormat,
    /// JPEG / HEIC / WebP quality, 1–100.
    pub jpeg_quality: u8,
    /// Copy EXIF / IPTC / XMP metadata to the new file.
    pub keep_metadata: bool,
    /// Drop the GPS location even when keeping metadata.
    pub remove_gps: bool,
    /// Give the new file the original's modification date.
    pub keep_date_modified: bool,
    /// Write into this folder instead of next to each original.
    pub destination: Option<PathBuf>,
}

impl Default for ResizeOptions {
    fn default() -> Self {
        Self {
            size: ResizeSize::default_presets().remove(0),
            shrink_only: false,
            ignore_orientation: true,
            replace: false,
            file_name_format: "%1 (%2)".into(),
            format: OutputFormat::Keep,
            jpeg_quality: 90,
            keep_metadata: true,
            remove_gps: false,
            keep_date_modified: false,
            destination: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Size math

/// A crop rectangle in the scaled image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The result of [`compute_dimensions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimensions {
    /// The size the whole image is scaled to.
    pub scaled_width: u32,
    pub scaled_height: u32,
    /// Fill mode: the centered part of the scaled image that is kept.
    pub crop: Option<CropRect>,
    /// "Only shrink" kept the original size.
    pub unchanged: bool,
}

impl Dimensions {
    /// The output image's size.
    pub fn output(&self) -> (u32, u32) {
        match self.crop {
            Some(c) => (c.width, c.height),
            None => (self.scaled_width, self.scaled_height),
        }
    }
}

fn validated(v: f64) -> anyhow::Result<u32> {
    anyhow::ensure!(
        v.is_finite() && v >= 0.0 && v <= f64::from(i32::MAX),
        "the size is out of range"
    );
    Ok((v.round_ties_even() as u32).max(1))
}

/// The output size for an image of `w` × `h` pixels at `dpi_x` / `dpi_y`
/// (as PowerToys' `CalculateDimensions`).
pub fn compute_dimensions(
    w: u32,
    h: u32,
    dpi_x: f64,
    dpi_y: f64,
    size: &ResizeSize,
    shrink_only: bool,
    ignore_orientation: bool,
) -> anyhow::Result<Dimensions> {
    anyhow::ensure!(w > 0 && h > 0, "the image is empty");
    let mut tw = size.pixel_width(w, dpi_x);
    let mut th = size.pixel_height(h, dpi_y);
    let unchanged = Dimensions {
        scaled_width: w,
        scaled_height: h,
        crop: None,
        unchanged: true,
    };

    if ignore_orientation && !size.has_auto() && size.unit != ResizeUnit::Percent {
        let (in_land, in_port) = (w > h, h > w);
        let (t_land, t_port) = (tw > th, th > tw);
        if (in_land && t_port) || (in_port && t_land) {
            std::mem::swap(&mut tw, &mut th);
        }
    }

    let mut sx = tw / f64::from(w);
    let mut sy = th / f64::from(h);
    match size.fit {
        ResizeFit::Fit => {
            sx = sx.min(sy);
            sy = sx;
        }
        ResizeFit::Fill => {
            sx = sx.max(sy);
            sy = sx;
        }
        ResizeFit::Stretch => {}
    }

    if shrink_only && size.unit != ResizeUnit::Percent {
        if sx > 1.0 || sy > 1.0 {
            return Ok(unchanged);
        }
        let crop_needed = size.fit == ResizeFit::Fill && (f64::from(w) > tw || f64::from(h) > th);
        if sx == 1.0 && sy == 1.0 && !crop_needed {
            return Ok(unchanged);
        }
    }

    let sw = validated(f64::from(w) * sx)?;
    let sh = validated(f64::from(h) * sy)?;
    if size.fit == ResizeFit::Fill {
        let tw = validated(tw)?;
        let th = validated(th)?;
        if sw > tw || sh > th {
            let (cw, ch) = (tw.min(sw), th.min(sh));
            return Ok(Dimensions {
                scaled_width: sw,
                scaled_height: sh,
                crop: Some(CropRect {
                    x: (sw - cw) / 2,
                    y: (sh - ch) / 2,
                    width: cw,
                    height: ch,
                }),
                unchanged: false,
            });
        }
    }
    Ok(Dimensions {
        scaled_width: sw,
        scaled_height: sh,
        crop: None,
        unchanged: false,
    })
}

// ---------------------------------------------------------------------------
// Naming

const RESERVED_WINDOWS_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Expand a PowerToys file name pattern: `%1` original name (without
/// extension), `%2` size name, `%3`/`%4` the selected width / height,
/// `%5`/`%6` the actual output width / height. Characters that aren't
/// allowed in file names become `_`.
pub fn format_file_name(
    pattern: &str,
    original_stem: &str,
    size: &ResizeSize,
    output: (u32, u32),
) -> String {
    let size_name = size.name.replace(['/', '\\'], "_");
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(d) = chars.peek().copied() {
                let v = match d {
                    '1' => Some(original_stem.to_string()),
                    '2' => Some(size_name.clone()),
                    '3' => Some(fmt_num(size.width)),
                    '4' => Some(fmt_num(size.height)),
                    '5' => Some(output.0.to_string()),
                    '6' => Some(output.1.to_string()),
                    _ => None,
                };
                if let Some(v) = v {
                    chars.next();
                    out.push_str(&v);
                    continue;
                }
            }
        }
        out.push(c);
    }
    let mut out: String = out
        .chars()
        .map(|c| match c {
            ':' | '*' | '?' | '"' | '<' | '>' | '|' | '/' | '\\' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = out.trim().to_string();
    out = if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        original_stem.to_string()
    } else {
        trimmed
    };
    if RESERVED_WINDOWS_NAMES.contains(&out.to_ascii_uppercase().as_str()) {
        out.push('_');
    }
    out
}

/// `stem.ext`, or `stem (1).ext`, `stem (2).ext` … when taken.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let name = |s: &str| {
        if ext.is_empty() {
            s.to_string()
        } else {
            format!("{s}.{ext}")
        }
    };
    let mut p = dir.join(name(stem));
    let mut n = 1;
    while p.symlink_metadata().is_ok() {
        p = dir.join(name(&format!("{stem} ({n})")));
        n += 1;
    }
    p
}

/// The format a file is written in: `opts.format`, or for "keep" the
/// input's format if it can be written (else JPEG).
pub fn resolve_format(requested: OutputFormat, input: Option<OutputFormat>) -> OutputFormat {
    match requested {
        OutputFormat::Keep => match input {
            Some(f) if available_formats().contains(&f) => f,
            _ => OutputFormat::Jpeg,
        },
        f => f,
    }
}

/// The extension of the output: the original's when it matches the format
/// (keeps `.JPEG` vs `.jpg`), else the format's.
pub fn output_extension(input: &Path, format: OutputFormat) -> String {
    let orig = input
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    if format
        .extensions()
        .contains(&orig.to_ascii_lowercase().as_str())
    {
        orig
    } else {
        format.extension().unwrap_or("jpg").to_string()
    }
}

/// Where the resized copy of `input` goes (not used with `replace`).
pub fn destination_path(
    input: &Path,
    opts: &ResizeOptions,
    format: OutputFormat,
    output: (u32, u32),
) -> PathBuf {
    let dir = opts
        .destination
        .clone()
        .or_else(|| input.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = format_file_name(&opts.file_name_format, &stem, &opts.size, output);
    unique_path(&dir, &name, &output_extension(input, format))
}

// ---------------------------------------------------------------------------
// Running

/// What a backend needs to write one image.
#[derive(Debug, Clone)]
pub struct EncodeJob<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub opts: &'a ResizeOptions,
    pub format: OutputFormat,
}

/// Size, resolution and format of an image file.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageInfo {
    /// As displayed (EXIF orientation applied).
    pub width: u32,
    pub height: u32,
    pub dpi_x: f64,
    pub dpi_y: f64,
    pub format: Option<OutputFormat>,
    /// EXIF orientation (1 = upright).
    pub orientation: u32,
    pub has_gps: bool,
    pub has_exif: bool,
}

/// Read an image's size, resolution, format and orientation.
pub fn image_info(path: &Path) -> anyhow::Result<ImageInfo> {
    #[cfg(target_os = "macos")]
    {
        mac::image_info(path)
    }
    #[cfg(windows)]
    {
        win::image_info(path)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = path;
        anyhow::bail!("image resizing isn't available on this platform")
    }
}

/// Whether `path` looks like an image the resizer can read (by extension).
pub fn is_image_path(path: &Path) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "jpg"
            | "jpeg"
            | "jpe"
            | "jfif"
            | "png"
            | "heic"
            | "heif"
            | "hif"
            | "webp"
            | "tif"
            | "tiff"
            | "gif"
            | "bmp"
            | "ico"
            | "jp2"
            | "avif"
            | "dng"
            | "cr2"
            | "cr3"
            | "nef"
            | "arw"
            | "raf"
            | "orf"
            | "rw2"
            | "psd"
            | "tga"
    )
}

/// Resize one image. Returns the written file.
pub fn resize_one(input: &Path, opts: &ResizeOptions) -> anyhow::Result<PathBuf> {
    let info = image_info(input)?;
    let dims = compute_dimensions(
        info.width,
        info.height,
        info.dpi_x,
        info.dpi_y,
        &opts.size,
        opts.shrink_only,
        opts.ignore_orientation,
    )?;
    let format = resolve_format(opts.format, info.format);
    let modified = std::fs::metadata(input).and_then(|m| m.modified()).ok();
    let out_dims = dims.output();

    let final_path = if opts.replace {
        let ext = output_extension(input, format);
        let same_name = input
            .extension()
            .is_some_and(|e| e.to_string_lossy() == ext);
        if same_name {
            input.to_path_buf()
        } else {
            // A different format gets the matching extension.
            let dir = input.parent().unwrap_or(Path::new("."));
            let stem = input
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            unique_path(dir, &stem, &ext)
        }
    } else {
        let p = destination_path(input, opts, format, out_dims);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        p
    };
    let dir = final_path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.pm-resize-{}.tmp",
        final_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    let job = EncodeJob {
        input,
        output: &tmp,
        opts,
        format,
    };
    let r = encode(&job, &info, &dims);
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // The original goes to the Trash (as PowerToys' Recycle Bin backup);
    // unit tests don't fill the real Trash.
    if opts.replace && !cfg!(test) {
        if let Err(e) = trash(input) {
            log::warn!(
                "{}: couldn't move to the Trash ({e}); overwriting",
                input.display()
            );
        }
    }
    if opts.replace && cfg!(test) && final_path != input {
        let _ = std::fs::remove_file(input);
    }
    if let Err(e) = std::fs::rename(&tmp, &final_path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    if opts.keep_date_modified {
        if let Some(t) = modified {
            if let Ok(f) = std::fs::File::options().write(true).open(&final_path) {
                let _ = f.set_modified(t);
            }
        }
    }
    Ok(final_path)
}

fn encode(job: &EncodeJob<'_>, info: &ImageInfo, dims: &Dimensions) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::encode(job, info, dims)
    }
    #[cfg(windows)]
    {
        win::encode(job, info, dims)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (job, info, dims);
        anyhow::bail!("image resizing isn't available on this platform")
    }
}

/// Move `path` to the Trash / Recycle Bin.
pub fn trash(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::trash(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        anyhow::bail!("no Trash on this platform")
    }
}

/// Resize every image in `paths` with `opts`, in order. Folders are
/// skipped (with an error); non-images fail when they can't be decoded.
pub fn resize(paths: &[PathBuf], opts: &ResizeOptions) -> Vec<anyhow::Result<PathBuf>> {
    resize_with_progress(paths, opts, &mut |_, _| {})
}

/// [`resize`] calling `progress(index, result)` after each file.
pub fn resize_with_progress(
    paths: &[PathBuf],
    opts: &ResizeOptions,
    progress: &mut dyn FnMut(usize, &anyhow::Result<PathBuf>),
) -> Vec<anyhow::Result<PathBuf>> {
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let r = if p.is_dir() {
                Err(anyhow::anyhow!("is a folder"))
            } else {
                resize_one(p, opts)
            };
            progress(i, &r);
            r
        })
        .collect()
}

/// Expand folders in `paths` to the images directly inside them.
pub fn collect_images(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                let mut v: Vec<PathBuf> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && is_image_path(p))
                    .collect();
                v.sort();
                out.extend(v);
            }
        } else {
            out.push(p.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests;
