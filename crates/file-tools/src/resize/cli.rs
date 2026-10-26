//! `powermac resize [options] <images or folders…>`.
//!
//! Without options it opens the Image Resizer window (what the Finder Quick
//! Action runs). Any option (or `--no-window`) resizes right away with the
//! remembered settings plus the options given, printing one line per file.
//!
//! ```text
//! --size small|medium|large|phone|<preset>|WxH|N%   e.g. 800x600, 800x0 (auto height), 50%
//! --width N  --height N  --unit px|%|cm|in  --fit fit|fill|stretch
//! --shrink-only  --keep-orientation  --replace  --name "%1 (%2)"
//! --format keep|jpg|png|heic|webp|tiff  --quality 1-100
//! --strip-metadata  --remove-gps  --keep-date  --out <folder>  --no-window
//! ```

use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _};

use super::{ResizeFit, ResizeOptions, ResizeSize, ResizeUnit};
use crate::config::{Config, ResizeConfig};

/// A parsed command line.
#[derive(Debug, Clone, PartialEq)]
pub struct ResizeCli {
    pub paths: Vec<PathBuf>,
    /// Resize right away (some option was given) instead of opening the window.
    pub direct: bool,
    pub opts: ResizeOptions,
}

/// Usage text.
pub const USAGE: &str = "usage: powermac resize [options] <images or folders…>
  Without options: opens the Image Resizer window.
  --size small|medium|large|phone|<preset>|WxH|N%   (800x600, 800x0 = auto height, 50%)
  --width N --height N --unit px|%|cm|in --fit fit|fill|stretch
  --shrink-only --keep-orientation --replace --name \"%1 (%2)\"
  --format keep|jpg|png|heic|webp|tiff --quality 1-100
  --strip-metadata --remove-gps --keep-date --out <folder> --no-window";

fn num(s: &str) -> anyhow::Result<f64> {
    let v: f64 = s
        .trim()
        .parse()
        .with_context(|| format!("not a number: {s}"))?;
    anyhow::ensure!(v.is_finite() && v >= 0.0, "not a valid size: {s}");
    Ok(v)
}

/// Parse `WxH`, `W×H`, `Nx0`, `N%` into (width, height, unit?).
pub fn parse_size_spec(s: &str) -> Option<(f64, f64, Option<ResizeUnit>)> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        let v = num(p).ok()?;
        return Some((v, v, Some(ResizeUnit::Percent)));
    }
    let lower = s.to_ascii_lowercase();
    let (mut body, mut unit) = (lower.as_str(), None);
    for u in ["px", "cm", "in"] {
        if let Some(b) = body.strip_suffix(u) {
            body = b.trim_end();
            unit = ResizeUnit::parse(u);
            break;
        }
    }
    let (w, h) = body.split_once(['x', '×', '*'])?;
    let auto = |t: &str| {
        let t = t.trim();
        if t.is_empty() || t == "auto" {
            Some(0.0)
        } else {
            num(t).ok()
        }
    };
    Some((auto(w)?, auto(h)?, unit))
}

/// Parse `args` (after `resize`) on top of the remembered settings.
pub fn parse(args: &[String], cfg: &ResizeConfig) -> anyhow::Result<ResizeCli> {
    let mut opts = cfg.current_options();
    let mut direct = false;
    let mut paths = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .cloned()
                .with_context(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--no-window" | "-y" => direct = true,
            "--size" | "-s" => {
                let v = val("--size")?;
                direct = true;
                let lv = v.to_ascii_lowercase();
                if let Some(p) = cfg
                    .presets
                    .iter()
                    .find(|p| p.name.to_ascii_lowercase() == lv)
                {
                    opts.size = p.clone();
                } else if let Some((w, h, u)) = parse_size_spec(&v) {
                    opts.size = ResizeSize {
                        name: "Custom".into(),
                        fit: opts.size.fit,
                        width: w,
                        height: h,
                        unit: u.unwrap_or(ResizeUnit::Pixel),
                    };
                } else {
                    bail!("unknown size: {v} (a preset name, WxH or N%)");
                }
            }
            "--width" | "-w" => {
                opts.size.width = num(&val("--width")?)?;
                opts.size.name = "Custom".into();
                direct = true;
            }
            "--height" | "-h" => {
                opts.size.height = num(&val("--height")?)?;
                opts.size.name = "Custom".into();
                direct = true;
            }
            "--unit" => {
                let v = val("--unit")?;
                opts.size.unit =
                    ResizeUnit::parse(&v).with_context(|| format!("unknown unit: {v}"))?;
                direct = true;
            }
            "--fit" => {
                let v = val("--fit")?;
                opts.size.fit =
                    ResizeFit::parse(&v).with_context(|| format!("unknown fit: {v}"))?;
                direct = true;
            }
            "--shrink-only" => {
                opts.shrink_only = true;
                direct = true;
            }
            "--keep-orientation" => {
                opts.ignore_orientation = false;
                direct = true;
            }
            "--ignore-orientation" => {
                opts.ignore_orientation = true;
                direct = true;
            }
            "--replace" => {
                opts.replace = true;
                direct = true;
            }
            "--name" => {
                opts.file_name_format = val("--name")?;
                direct = true;
            }
            "--format" | "-f" => {
                let v = val("--format")?;
                opts.format = super::OutputFormat::parse(&v)
                    .with_context(|| format!("unknown format: {v}"))?;
                direct = true;
            }
            "--quality" | "-q" => {
                let v = val("--quality")?;
                let q: u8 = v.parse().with_context(|| format!("not a quality: {v}"))?;
                opts.jpeg_quality = q.clamp(1, 100);
                direct = true;
            }
            "--strip-metadata" => {
                opts.keep_metadata = false;
                direct = true;
            }
            "--keep-metadata" => {
                opts.keep_metadata = true;
                direct = true;
            }
            "--remove-gps" => {
                opts.remove_gps = true;
                direct = true;
            }
            "--keep-date" => {
                opts.keep_date_modified = true;
                direct = true;
            }
            "--out" | "-o" => {
                opts.destination = Some(PathBuf::from(val("--out")?));
                direct = true;
            }
            "--help" => bail!("{USAGE}"),
            "--" => paths.extend(it.by_ref().map(PathBuf::from)),
            s if s.starts_with('-') && s.len() > 1 => bail!("unknown option {s}\n{USAGE}"),
            s => paths.push(PathBuf::from(s)),
        }
    }
    Ok(ResizeCli {
        paths,
        direct,
        opts,
    })
}

/// Run `powermac resize <args>` with settings from `cfg_path` (default:
/// [`Config::default_path`]). Opens the window (blocking; main thread) or
/// resizes directly and prints a line per file. Returns the process exit
/// code (1 when any file failed).
pub fn run(args: &[String]) -> anyhow::Result<i32> {
    run_with(args, Config::default_path().as_deref())
}

/// [`run`] with an explicit settings file.
pub fn run_with(args: &[String], cfg_path: Option<&Path>) -> anyhow::Result<i32> {
    let cfg = cfg_path.map(Config::load_from).unwrap_or_default();
    let cli = parse(args, &cfg.resize)?;
    anyhow::ensure!(!cli.paths.is_empty(), "{USAGE}");
    if !cli.direct {
        crate::resize_ui::run_resize_window_with(cli.paths, cfg_path.map(Path::to_path_buf))?;
        return Ok(0);
    }
    let files = super::collect_images(&cli.paths);
    let mut failed = 0;
    for (p, r) in files.iter().zip(super::resize(&files, &cli.opts)) {
        match r {
            Ok(out) => println!("{} -> {}", p.display(), out.display()),
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e:#}", p.display());
            }
        }
    }
    Ok(i32::from(failed > 0))
}
