//! Color conversions and color format templates, ported from PowerToys'
//! `ManagedCommon.ColorFormatHelper` / `ColorNameHelper` so that every format
//! produces exactly the text PowerToys Color Picker produces (including .NET's
//! banker's rounding and `Color.GetHue()` single-precision math).
//!
//! A format is a template such as `rgb(%Re, %Gr, %Bl)`: `%` followed by a
//! two-letter parameter and an optional one-letter formatter
//! (`%Rex` = red as two lowercase hex digits). See [`PARAMETERS`] and
//! [`FORMATTERS`]; `%Na` is the color name.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An sRGB color (8 bits per channel, opaque).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// `#RRGGBB` (upper case). This is also the serialized form.
    pub fn hex(&self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// Parse `#RRGGBB`, `RRGGBB`, `#RGB`, `RGB` or `0xRRGGBB` / `0xFFRRGGBB`.
    pub fn from_hex(s: &str) -> Option<Rgb> {
        let s = s.trim();
        let s = s
            .strip_prefix('#')
            .or_else(|| s.strip_prefix("0x"))
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        if !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
        match s.len() {
            3 => {
                let n = |i: usize| u8::from_str_radix(&s[i..i + 1], 16).ok().map(|v| v * 17);
                Some(Rgb::new(n(0)?, n(1)?, n(2)?))
            }
            6 => Some(Rgb::new(byte(0)?, byte(2)?, byte(4)?)),
            8 => Some(Rgb::new(byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    }

    /// Relative luminance (WCAG), 0..=1.
    pub fn luminance(&self) -> f64 {
        let (r, g, b) = srgb_to_linear(
            self.r as f64 / 255.0,
            self.g as f64 / 255.0,
            self.b as f64 / 255.0,
        );
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// Whether dark text reads better than white text on this color.
    pub fn is_light(&self) -> bool {
        self.luminance() > 0.36
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgb::from_hex(&s).ok_or_else(|| serde::de::Error::custom(format!("bad color {s:?}")))
    }
}

// ---------------------------------------------------------------------------
// .NET numeric helpers

/// `Math.Round(x)` (MidpointRounding.ToEven).
fn round_even(x: f64) -> f64 {
    x.round_ties_even()
}

/// `Math.Round(x, digits)` (ToEven), the way .NET computes it.
fn round_digits(x: f64, digits: i32) -> f64 {
    if x.abs() < 1e16 {
        let p = 10f64.powi(digits);
        round_even(x * p) / p
    } else {
        x
    }
}

/// `double.ToString(CultureInfo.InvariantCulture)` for the values we print
/// (shortest round-trip; never `-0`).
fn num(x: f64) -> String {
    let x = if x == 0.0 { 0.0 } else { x };
    format!("{x}")
}

/// .NET `System.Drawing.Color.GetHue()` (single precision), in degrees.
pub fn hue(c: Rgb) -> f64 {
    let (r, g, b) = (c.r as i32, c.g as i32, c.b as i32);
    if r == g && g == b {
        return 0.0;
    }
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = (max - min) as f32;
    let mut h: f32 = if r == max {
        (g - b) as f32 / delta
    } else if g == max {
        (b - r) as f32 / delta + 2.0
    } else {
        (r - g) as f32 / delta + 4.0
    };
    h *= 60.0;
    if h < 0.0 {
        h += 360.0;
    }
    h as f64
}

fn min_max(c: Rgb) -> (f64, f64) {
    let min = c.r.min(c.g).min(c.b) as f64 / 255.0;
    let max = c.r.max(c.g).max(c.b) as f64 / 255.0;
    (min, max)
}

// ---------------------------------------------------------------------------
// Conversions (ColorFormatHelper)

/// Cyan, magenta, yellow, black key — each 0..=1.
pub fn to_cmyk(c: Rgb) -> (f64, f64, f64, f64) {
    if c.r == 0 && c.g == 0 && c.b == 0 {
        return (0.0, 0.0, 0.0, 1.0);
    }
    let (r, g, b) = (c.r as f64 / 255.0, c.g as f64 / 255.0, c.b as f64 / 255.0);
    let k = 1.0 - r.max(g).max(b);
    if 1.0 - k == 0.0 {
        return (0.0, 0.0, 0.0, 1.0);
    }
    (
        (1.0 - r - k) / (1.0 - k),
        (1.0 - g - k) / (1.0 - k),
        (1.0 - b - k) / (1.0 - k),
        k,
    )
}

/// Hue (degrees), saturation and value (0..=1). HSB is the same space.
pub fn to_hsv(c: Rgb) -> (f64, f64, f64) {
    let (min, max) = min_max(c);
    (
        hue(c),
        if max == 0.0 { 0.0 } else { (max - min) / max },
        max,
    )
}

/// Hue (degrees), saturation and intensity (0..=1).
pub fn to_hsi(c: Rgb) -> (f64, f64, f64) {
    if c.r == 0 && c.g == 0 && c.b == 0 {
        return (0.0, 0.0, 0.0);
    }
    let i = (c.r as f64 / 255.0 + c.g as f64 / 255.0 + c.b as f64 / 255.0) / 3.0;
    let (min, _) = min_max(c);
    (hue(c), 1.0 - min / i, i)
}

/// Hue (degrees), saturation and lightness (0..=1).
pub fn to_hsl(c: Rgb) -> (f64, f64, f64) {
    let (min, max) = min_max(c);
    let l = (max + min) / 2.0;
    if l == 0.0 || min == max {
        (hue(c), 0.0, l)
    } else if l > 0.0 && l <= 0.5 {
        (hue(c), (max - min) / (max + min), l)
    } else {
        (hue(c), (max - min) / (2.0 - (max + min)), l)
    }
}

/// Hue (degrees), whiteness and blackness (0..=1).
pub fn to_hwb(c: Rgb) -> (f64, f64, f64) {
    let (min, max) = min_max(c);
    (hue(c), min, 1.0 - max)
}

/// Natural color (NCol) hue such as `R24`, whiteness, blackness.
pub fn to_ncol(c: Rgb) -> (String, f64, f64) {
    let (min, max) = min_max(c);
    let h = hue(c);
    let (letter, base) = if h < 60.0 {
        ('R', 0.0)
    } else if h < 120.0 {
        ('Y', 60.0)
    } else if h < 180.0 {
        ('G', 120.0)
    } else if h < 240.0 {
        ('C', 180.0)
    } else if h < 300.0 {
        ('B', 240.0)
    } else {
        ('M', 300.0)
    };
    (
        format!("{letter}{}", num(round_even((h - base) / 0.6))),
        min,
        1.0 - max,
    )
}

pub fn srgb_to_linear(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let f = |v: f64| {
        if v > 0.04045 {
            ((v + 0.055) / 1.055).powf(2.4)
        } else {
            v / 12.92
        }
    };
    (f(r), f(g), f(b))
}

/// CIE XYZ (D65), each roughly 0..=1.
#[allow(clippy::excessive_precision)] // PowerToys' constants, verbatim
pub fn to_xyz(c: Rgb) -> (f64, f64, f64) {
    let (r, g, b) = srgb_to_linear(c.r as f64 / 255.0, c.g as f64 / 255.0, c.b as f64 / 255.0);
    (
        r * 0.41239079926595948 + g * 0.35758433938387796 + b * 0.18048078840183429,
        r * 0.21263900587151036 + g * 0.71516867876775593 + b * 0.07219231536073372,
        r * 0.01933081871559185 + g * 0.11919477979462599 + b * 0.95053215224966058,
    )
}

/// CIE L*a*b* (D65): lightness 0..=100, a and b about -128..=127.
pub fn to_lab(c: Rgb) -> (f64, f64, f64) {
    let (x, y, z) = to_xyz(c);
    let (x, y, z) = (x / 0.9504559270516717, y / 1.0, z / 1.0890577507598784);
    let delta: f64 = 6.0 / 29.0;
    let m = (1.0 / 3.0) * delta.powf(-2.0);
    let t = delta.powi(3);
    let f = |v: f64| {
        if v > t {
            v.powf(1.0 / 3.0)
        } else {
            v * m + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

/// Oklab: lightness 0..=1, a and b about -0.5..=0.5.
#[allow(clippy::excessive_precision)]
pub fn to_oklab(c: Rgb) -> (f64, f64, f64) {
    let (r, g, b) = srgb_to_linear(c.r as f64 / 255.0, c.g as f64 / 255.0, c.b as f64 / 255.0);
    let l = 0.41222147079999993 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034981999999 * r + 0.6806995450999999 * g + 0.1073969566 * b;
    let s = 0.08830246189999998 * r + 0.2817188376 * g + 0.6299787005000002 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    (
        0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
    )
}

/// Oklch: lightness 0..=1, chroma 0..=0.5, hue (degrees).
pub fn to_oklch(c: Rgb) -> (f64, f64, f64) {
    let (l, a, b) = to_oklab(c);
    let chroma = (a * a + b * b).sqrt();
    let h = if round_digits(chroma, 3) == 0.0 {
        0.0
    } else {
        (b.atan2(a) * 180.0 / std::f64::consts::PI + 360.0) % 360.0
    };
    (l, chroma, h)
}

/// HSV → RGB the way PowerToys' editor does it (`HSVColor.RGBFromHSV`,
/// truncating to bytes). Out-of-range input gives black.
pub fn from_hsv_truncating(h: f64, s: f64, v: f64) -> Rgb {
    if !(0.0..=360.0).contains(&h) || !(0.0..=1.0).contains(&s) || !(0.0..=1.0).contains(&v) {
        return Rgb::new(0, 0, 0);
    }
    let (r, g, b) = hsv_components(h, s, v);
    let to = |x: f64| (x * 255.0) as u8;
    Rgb::new(to(r), to(g), to(b))
}

/// HSV → RGB with rounding (for the editor's sliders, so a round trip is stable).
pub fn from_hsv(h: f64, s: f64, v: f64) -> Rgb {
    let h = h.rem_euclid(360.0);
    let (r, g, b) = hsv_components(h, s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let to = |x: f64| (x * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgb::new(to(r), to(g), to(b))
}

fn hsv_components(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    (r + m, g + m, b + m)
}

/// The four variations PowerToys' editor shows around a color: two lighter
/// (+0.3 and +0.15 value) and two darker (-0.2 and -0.3 value), with a small
/// hue shift at the ends of the range.
pub fn variations(c: Rgb) -> [Rgb; 4] {
    let (h, s, v) = to_hsv(c);
    let k1 = if 1.0 - v < 0.15 { 1.0 } else { 0.0 };
    let k2 = if v - 0.3 < 0.0 { 1.0 } else { 0.0 };
    [
        from_hsv_truncating((h + k1 * 8.0).min(360.0), s, (v + 0.3).min(1.0)),
        from_hsv_truncating((h + k1 * 4.0).min(360.0), s, (v + 0.15).min(1.0)),
        from_hsv_truncating((h - k2 * 4.0).max(0.0), s, (v - 0.2).max(0.0)),
        from_hsv_truncating((h - k2 * 8.0).max(0.0), s, (v - 0.3).max(0.0)),
    ]
}

/// Tints (towards white, `t` > 0) and shades (towards black, `t` < 0) in
/// linear-light-free sRGB, like CSS `color-mix`.
pub fn mix_white_black(c: Rgb, t: f64) -> Rgb {
    let (target, t) = if t >= 0.0 { (255.0, t) } else { (0.0, -t) };
    let f = |v: u8| (v as f64 + (target - v as f64) * t.clamp(0.0, 1.0)).round() as u8;
    Rgb::new(f(c.r), f(c.g), f(c.b))
}

// ---------------------------------------------------------------------------
// Templates

/// Template parameters: (name, default formatter, description).
pub const PARAMETERS: &[(&str, char, &str)] = &[
    ("Re", 'b', "red"),
    ("Gr", 'b', "green"),
    ("Bl", 'b', "blue"),
    ("Al", 'b', "alpha"),
    ("Cy", 'p', "cyan"),
    ("Ma", 'p', "magenta"),
    ("Ye", 'p', "yellow"),
    ("Bk", 'p', "black key"),
    ("Hu", 'i', "hue"),
    ("Hn", 'i', "hue (natural)"),
    ("Si", 'p', "saturation (HSI)"),
    ("Sl", 'p', "saturation (HSL)"),
    ("Sb", 'p', "saturation (HSB)"),
    ("Br", 'p', "brightness"),
    ("In", 'p', "intensity"),
    ("Ll", 'p', "lightness (HSL)"),
    ("Va", 'p', "value"),
    ("Wh", 'p', "whiteness"),
    ("Bn", 'p', "blackness"),
    ("Lc", 'p', "lightness (CIE)"),
    ("Ca", 'p', "chromaticity A (CIELAB)"),
    ("Cb", 'p', "chromaticity B (CIELAB)"),
    ("Lo", 'p', "lightness (Oklab/Oklch)"),
    ("Oa", 'p', "chromaticity A (Oklab)"),
    ("Ob", 'p', "chromaticity B (Oklab)"),
    ("Oc", 'p', "chroma (Oklch)"),
    ("Oh", 'p', "hue angle (Oklch)"),
    ("Xv", 'i', "X value"),
    ("Yv", 'i', "Y value"),
    ("Zv", 'i', "Z value"),
    ("Dr", 'i', "decimal value (RGB)"),
    ("Dv", 'i', "decimal value (BGR)"),
];

/// Formatters that may follow a parameter: (char, description).
pub const FORMATTERS: &[(char, &str)] = &[
    ('b', "byte 0..255"),
    ('h', "hex digit, lower case"),
    ('H', "hex digit, upper case"),
    ('x', "two hex digits, lower case"),
    ('X', "two hex digits, upper case"),
    ('f', "float 0..1 with leading zero"),
    ('F', "float 0..1 without leading zero"),
    ('p', "percent"),
    ('i', "integer"),
    ('s', "string"),
];

/// The color name placeholder.
pub const NAME_PARAMETER: &str = "%Na";

/// Built-in formats in PowerToys order, plus Apple code formats.
pub const BUILTIN_FORMATS: &[(&str, &str)] = &[
    ("HEX", "%Rex%Grx%Blx"),
    ("RGB", "rgb(%Re, %Gr, %Bl)"),
    ("HSL", "hsl(%Hu, %Sl%, %Ll%)"),
    ("HSV", "hsv(%Hu, %Sb%, %Va%)"),
    ("CMYK", "cmyk(%Cy%, %Ma%, %Ye%, %Bk%)"),
    ("HSB", "hsb(%Hu, %Sb%, %Br%)"),
    ("HSI", "hsi(%Hu, %Si%, %In%)"),
    ("HWB", "hwb(%Hu, %Wh%, %Bn%)"),
    ("NCol", "%Hn, %Wh%, %Bn%"),
    ("CIEXYZ", "XYZ(%Xv, %Yv, %Zv)"),
    ("CIELAB", "CIELab(%Lc, %Ca, %Cb)"),
    ("Oklab", "oklab(%Lo, %Oa, %Ob)"),
    ("Oklch", "oklch(%Lo, %Oc, %Oh)"),
    ("VEC4", "(%Reff, %Grff, %Blff, 1f)"),
    ("Decimal", "%Dv"),
    ("HEX Int", "0xFF%ReX%GrX%BlX"),
    ("SwiftUI", "Color(red: %Ref, green: %Grf, blue: %Blf)"),
    (
        "NSColor",
        "NSColor(srgbRed: %Ref, green: %Grf, blue: %Blf, alpha: 1)",
    ),
    (
        "UIColor",
        "UIColor(red: %Ref, green: %Grf, blue: %Blf, alpha: 1)",
    ),
    (
        "Swift literal",
        "#colorLiteral(red: %Ref, green: %Grf, blue: %Blf, alpha: 1)",
    ),
];

/// The template of a built-in format.
pub fn builtin_format(name: &str) -> Option<&'static str> {
    BUILTIN_FORMATS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, f)| *f)
}

fn default_formatter(param: &str) -> Option<char> {
    PARAMETERS
        .iter()
        .find(|(p, ..)| *p == param)
        .map(|(_, f, _)| *f)
}

fn is_formatter(c: char) -> bool {
    FORMATTERS.iter().any(|(f, _)| *f == c)
}

fn byte_formatted(v: u8, f: char) -> String {
    match f {
        'h' => format!("{:x}", v / 16),
        'H' => format!("{:X}", v / 16),
        'x' => format!("{v:02x}"),
        'X' => format!("{v:02X}"),
        'f' | 'F' => {
            // .NET "0.##" / ".##"
            let s = format!("{:.2}", v as f64 / 255.0);
            let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
            if f == 'F' {
                if s == "0" {
                    String::new()
                } else {
                    s.strip_prefix('0').map(str::to_string).unwrap_or(s)
                }
            } else {
                s
            }
        }
        _ => v.to_string(),
    }
}

fn percent_formatted(v: f64, f: char, digits: i32) -> String {
    if f == 'i' {
        num(round_even(v))
    } else {
        num(round_digits(v, digits))
    }
}

fn parameter(c: Rgb, f: char, param: &str) -> String {
    let pct = |x: f64| num(round_even(x * 100.0));
    match param {
        "Re" => byte_formatted(c.r, f),
        "Gr" => byte_formatted(c.g, f),
        "Bl" => byte_formatted(c.b, f),
        "Al" => byte_formatted(255, f),
        "Cy" => pct(to_cmyk(c).0),
        "Ma" => pct(to_cmyk(c).1),
        "Ye" => pct(to_cmyk(c).2),
        "Bk" => pct(to_cmyk(c).3),
        "Hu" => num(round_even(to_hsv(c).0)),
        "Hn" => to_ncol(c).0,
        "Sb" => pct(to_hsv(c).1),
        "Si" => pct(to_hsi(c).1),
        "Sl" => pct(to_hsl(c).1),
        "Va" | "Br" => pct(to_hsv(c).2),
        "In" => pct(to_hsi(c).2),
        "Ll" => pct(to_hsl(c).2),
        "Lc" => percent_formatted(to_lab(c).0, f, 2),
        "Lo" => num(round_digits(to_oklab(c).0, 2)),
        "Wh" => pct(to_hwb(c).1),
        "Bn" => pct(to_hwb(c).2),
        "Ca" => percent_formatted(to_lab(c).1, f, 2),
        "Cb" => percent_formatted(to_lab(c).2, f, 2),
        "Oa" => num(round_digits(to_oklab(c).1, 2)),
        "Ob" => num(round_digits(to_oklab(c).2, 2)),
        "Oc" => num(round_digits(to_oklch(c).1, 2)),
        "Oh" => num(round_digits(to_oklch(c).2, 2)),
        "Xv" => num(round_digits(to_xyz(c).0 * 100.0, 4)),
        "Yv" => num(round_digits(to_xyz(c).1 * 100.0, 4)),
        "Zv" => num(round_digits(to_xyz(c).2 * 100.0, 4)),
        "Dr" => (c.r as u32 * 65536 + c.g as u32 * 256 + c.b as u32).to_string(),
        "Dv" => (c.r as u32 + c.g as u32 * 256 + c.b as u32 * 65536).to_string(),
        _ => String::new(),
    }
}

/// Expand a format template for `c` (PowerToys
/// `ColorRepresentationHelper.GetStringRepresentation`). An empty template
/// gives lower-case hex.
pub fn format_color(c: Rgb, template: &str) -> String {
    if template.is_empty() {
        return format!("{:02x}{:02x}{:02x}", c.r, c.g, c.b);
    }
    let mut s: Vec<char> = template.chars().collect();
    let find = |s: &[char], from: usize| {
        s.iter()
            .skip(from)
            .position(|&ch| ch == '%')
            .map(|i| i + from)
    };
    let mut pos = find(&s, 0);
    while let Some(p) = pos {
        if p + 2 >= s.len() {
            break;
        }
        let param: String = s[p + 1..p + 3].iter().collect();
        if let Some(default) = default_formatter(&param) {
            let (f, count) = if p + 3 >= s.len() {
                (default, 2)
            } else if is_formatter(s[p + 3]) {
                (s[p + 3], 3)
            } else {
                (default, 2)
            };
            let rep: Vec<char> = parameter(c, f, &param).chars().collect();
            s.splice(p..p + count + 1, rep);
        }
        pos = find(&s, p + 1);
    }
    let out: String = s.into_iter().collect();
    if out.contains(NAME_PARAMETER) {
        out.replace(NAME_PARAMETER, color_name(c))
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// Color names (ColorNameHelper, English strings)

const HUE_LIMITS: [[i32; 23]; 5] = [
    [
        8, 0, 0, 44, 0, 0, 0, 63, 0, 0, 122, 0, 134, 0, 0, 0, 0, 166, 176, 241, 0, 256, 0,
    ],
    [
        0, 10, 0, 32, 46, 0, 0, 0, 61, 0, 106, 0, 136, 144, 0, 0, 0, 158, 166, 241, 0, 0, 256,
    ],
    [
        0, 8, 0, 0, 39, 46, 0, 0, 0, 71, 120, 0, 131, 144, 0, 0, 163, 0, 177, 211, 249, 0, 256,
    ],
    [
        0, 11, 26, 0, 0, 38, 45, 0, 0, 56, 100, 121, 129, 0, 140, 0, 180, 0, 0, 224, 241, 0, 256,
    ],
    [
        0, 13, 27, 0, 0, 36, 45, 0, 0, 59, 118, 0, 127, 136, 142, 0, 185, 0, 0, 216, 239, 0, 256,
    ],
];
const LUM_LOW: [i32; 23] = [
    130, 100, 115, 100, 100, 100, 110, 75, 100, 90, 100, 100, 100, 100, 80, 100, 100, 100, 100,
    100, 100, 100, 100,
];
const LUM_HIGH: [i32; 23] = [
    170, 170, 170, 155, 170, 170, 170, 170, 170, 115, 170, 170, 170, 170, 170, 170, 170, 170, 150,
    150, 170, 140, 165,
];
const NAMES_LIGHT: [&str; 23] = [
    "Coral",
    "Rose",
    "Light orange",
    "Tan",
    "Tan",
    "Light yellow",
    "Light yellow",
    "Tan",
    "Light green",
    "Lime",
    "Light green",
    "Light green",
    "Aqua",
    "Sky blue",
    "Light turquoise",
    "Pale blue",
    "Light blue",
    "Ice blue",
    "Periwinkle",
    "Lavender",
    "Pink",
    "Tan",
    "Rose",
];
const NAMES_MID: [&str; 23] = [
    "Coral",
    "Red",
    "Orange",
    "Brown",
    "Tan",
    "Gold",
    "Yellow",
    "Olive green",
    "Olive green",
    "Green",
    "Green",
    "Bright green",
    "Teal",
    "Aqua",
    "Turquoise",
    "Pale blue",
    "Blue",
    "Blue gray",
    "Indigo",
    "Purple",
    "Pink",
    "Brown",
    "Red",
];
const NAMES_DARK: [&str; 23] = [
    "Brown",
    "Dark red",
    "Brown",
    "Brown",
    "Brown",
    "Dark yellow",
    "Dark yellow",
    "Brown",
    "Dark green",
    "Dark green",
    "Dark green",
    "Dark green",
    "Dark teal",
    "Dark teal",
    "Dark teal",
    "Dark blue",
    "Dark blue",
    "Blue gray",
    "Indigo",
    "Dark purple",
    "Plum",
    "Brown",
    "Dark red",
];

/// A rough English name for `c` ("Light blue", "Dark gray", …), as PowerToys shows it.
pub fn color_name(c: Rgb) -> &'static str {
    let (h, s, l) = to_hsl(c);
    let hue = (if h == 0.0 { 0.0 } else { h / 360.0 }) * 255.0;
    let sat = s * 255.0;
    let lum = l * 255.0;
    if lum > 240.0 {
        return "White";
    } else if lum < 20.0 {
        return "Black";
    }
    if sat <= 20.0 {
        return if lum > 170.0 {
            "Light gray"
        } else if lum > 100.0 {
            "Gray"
        } else {
            "Dark gray"
        };
    }
    let limits = if sat <= 75.0 {
        &HUE_LIMITS[0]
    } else if sat <= 115.0 {
        &HUE_LIMITS[1]
    } else if sat <= 150.0 {
        &HUE_LIMITS[2]
    } else if sat <= 240.0 {
        &HUE_LIMITS[3]
    } else {
        &HUE_LIMITS[4]
    };
    match (0..23).find(|&i| hue < limits[i] as f64) {
        Some(i) if lum > LUM_HIGH[i] as f64 => NAMES_LIGHT[i],
        Some(i) if lum < LUM_LOW[i] as f64 => NAMES_DARK[i],
        Some(i) => NAMES_MID[i],
        None => "",
    }
}
