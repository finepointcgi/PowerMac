use super::*;

fn px(w: f64, h: f64, fit: ResizeFit) -> ResizeSize {
    ResizeSize::new("Test", fit, w, h, ResizeUnit::Pixel)
}

fn dims(w: u32, h: u32, s: &ResizeSize, shrink: bool, orient: bool) -> Dimensions {
    compute_dimensions(w, h, 96.0, 96.0, s, shrink, orient).unwrap()
}

#[test]
fn fit_fill_stretch() {
    let small = &ResizeSize::default_presets()[0];
    // Fit: the smaller scale wins (1920x1080 → 853.33 x 480).
    let d = dims(1920, 1080, small, false, true);
    assert_eq!(d.output(), (853, 480));
    assert!(d.crop.is_none() && !d.unchanged);
    // Fill: the larger scale, then a centered crop.
    let d = dims(1000, 1000, &px(854.0, 480.0, ResizeFit::Fill), false, false);
    assert_eq!((d.scaled_width, d.scaled_height), (854, 854));
    assert_eq!(
        d.crop,
        Some(CropRect {
            x: 0,
            y: 187,
            width: 854,
            height: 480
        })
    );
    assert_eq!(d.output(), (854, 480));
    // Fill without overflow: no crop.
    let d = dims(1920, 1080, &px(960.0, 540.0, ResizeFit::Fill), false, false);
    assert_eq!(d.output(), (960, 540));
    assert!(d.crop.is_none());
    // Stretch: each side on its own.
    let d = dims(
        1000,
        500,
        &px(300.0, 300.0, ResizeFit::Stretch),
        false,
        false,
    );
    assert_eq!(d.output(), (300, 300));
    // Upscaling is allowed without "only shrink".
    let d = dims(100, 50, &px(400.0, 400.0, ResizeFit::Fit), false, false);
    assert_eq!(d.output(), (400, 200));
}

#[test]
fn units() {
    let pct = ResizeSize::new("Half", ResizeFit::Fit, 50.0, 0.0, ResizeUnit::Percent);
    assert_eq!(dims(1000, 800, &pct, false, true).output(), (500, 400));
    // Percent + fit uses the width's percentage for both sides.
    let pct = ResizeSize::new("Half", ResizeFit::Fit, 50.0, 10.0, ResizeUnit::Percent);
    assert_eq!(dims(1000, 800, &pct, false, true).output(), (500, 400));
    // Percent + stretch: separately.
    let pct = ResizeSize::new("P", ResizeFit::Stretch, 50.0, 25.0, ResizeUnit::Percent);
    assert_eq!(dims(1000, 800, &pct, false, true).output(), (500, 200));
    // Centimeters and inches use the image's DPI.
    let cm = ResizeSize::new("cm", ResizeFit::Stretch, 2.54, 5.08, ResizeUnit::Centimeter);
    let d = compute_dimensions(1000, 1000, 300.0, 300.0, &cm, false, false).unwrap();
    assert_eq!(d.output(), (300, 600));
    let inch = ResizeSize::new("in", ResizeFit::Stretch, 2.0, 1.0, ResizeUnit::Inch);
    let d = compute_dimensions(1000, 1000, 72.0, 72.0, &inch, false, false).unwrap();
    assert_eq!(d.output(), (144, 72));
    // A missing DPI counts as 72.
    let d = compute_dimensions(1000, 1000, 0.0, 0.0, &inch, false, false).unwrap();
    assert_eq!(d.output(), (144, 72));
    assert_eq!(ResizeUnit::parse("%"), Some(ResizeUnit::Percent));
    assert_eq!(ResizeUnit::parse("inches"), Some(ResizeUnit::Inch));
    assert_eq!(ResizeUnit::parse("furlong"), None);
}

#[test]
fn auto_sides() {
    // Fit with auto width: only the height constrains.
    let d = dims(1200, 800, &px(0.0, 600.0, ResizeFit::Fit), false, true);
    assert_eq!(d.output(), (900, 600));
    let d = dims(1200, 800, &px(300.0, 0.0, ResizeFit::Fit), false, true);
    assert_eq!(d.output(), (300, 200));
    // Fill with auto width keeps the original width as the box width.
    let d = dims(1200, 800, &px(0.0, 600.0, ResizeFit::Fill), false, true);
    assert_eq!(d.output(), (1200, 600));
    assert_eq!(d.crop.unwrap().y, 100);
    // Orientation swapping is off with an auto side.
    let d = dims(800, 1200, &px(300.0, 0.0, ResizeFit::Fit), false, true);
    assert_eq!(d.output(), (300, 450));
}

#[test]
fn only_shrink() {
    let small = &ResizeSize::default_presets()[0];
    // Smaller than the box: unchanged.
    let d = dims(640, 360, small, true, true);
    assert!(d.unchanged);
    assert_eq!(d.output(), (640, 360));
    // Exactly the box: unchanged.
    assert!(dims(854, 480, small, true, true).unchanged);
    // Larger: shrinks.
    let d = dims(1708, 960, small, true, true);
    assert_eq!(d.output(), (854, 480));
    // Stretch that would enlarge one side: unchanged.
    let d = dims(
        1000,
        100,
        &px(500.0, 200.0, ResizeFit::Stretch),
        true,
        false,
    );
    assert!(d.unchanged);
    // Percent ignores "only shrink" (as PowerToys).
    let pct = ResizeSize::new("2x", ResizeFit::Fit, 200.0, 0.0, ResizeUnit::Percent);
    assert_eq!(dims(100, 50, &pct, true, true).output(), (200, 100));
    // Fill that only needs a crop at scale 1 still crops.
    let d = dims(1000, 600, &px(1000.0, 500.0, ResizeFit::Fill), true, false);
    assert!(!d.unchanged);
    assert_eq!(d.output(), (1000, 500));
}

#[test]
fn orientation() {
    let small = &ResizeSize::default_presets()[0];
    // Portrait picture, landscape box: the box turns with "ignore orientation".
    assert_eq!(dims(1080, 1920, small, false, true).output(), (480, 853));
    assert_eq!(dims(1080, 1920, small, false, false).output(), (270, 480));
    // The phone preset (portrait) for a landscape picture.
    let phone = &ResizeSize::default_presets()[3];
    assert_eq!(dims(1136, 640, phone, false, true).output(), (568, 320));
    // Square pictures don't swap.
    assert_eq!(dims(1000, 1000, phone, false, true).output(), (320, 320));
}

#[test]
fn rounding_and_limits() {
    // .NET Math.Round: ties to even (2.5 → 2, 3.5 → 4) and at least 1 px.
    let s = px(5.0, 1000.0, ResizeFit::Stretch);
    let d = dims(
        10,
        10,
        &ResizeSize::new("r", ResizeFit::Stretch, 25.0, 35.0, ResizeUnit::Percent),
        false,
        false,
    );
    assert_eq!(d.output(), (2, 4));
    let _ = s;
    let d = dims(1000, 1, &px(10.0, 0.0, ResizeFit::Fit), false, false);
    assert_eq!(d.output(), (10, 1));
    assert!(compute_dimensions(
        0,
        10,
        72.0,
        72.0,
        &px(1.0, 1.0, ResizeFit::Fit),
        false,
        false
    )
    .is_err());
    assert!(compute_dimensions(
        10,
        10,
        72.0,
        72.0,
        &px(1e12, 1e12, ResizeFit::Stretch),
        false,
        false
    )
    .is_err());
}

#[test]
fn naming() {
    let small = &ResizeSize::default_presets()[0];
    assert_eq!(
        format_file_name("%1 (%2)", "IMG_1", small, (853, 480)),
        "IMG_1 (Small)"
    );
    assert_eq!(
        format_file_name("%1_%3x%4_%5x%6", "IMG", small, (853, 480)),
        "IMG_854x480_853x480"
    );
    let odd = ResizeSize::new(
        "1/2 \\ size",
        ResizeFit::Fit,
        12.5,
        0.0,
        ResizeUnit::Percent,
    );
    assert_eq!(
        format_file_name("%1 (%2) %3", "a", &odd, (1, 1)),
        "a (1_2 _ size) 12.5"
    );
    assert_eq!(format_file_name("%1: %7?", "x", small, (1, 1)), "x_ %7_");
    assert_eq!(format_file_name("%1", "con", small, (1, 1)), "con_");
    assert_eq!(format_file_name("  ", "orig", small, (1, 1)), "orig");
    assert_eq!(format_file_name("100%", "a", small, (1, 1)), "100%");

    let d = tempfile::tempdir().unwrap();
    assert_eq!(
        unique_path(d.path(), "a (Small)", "jpg"),
        d.path().join("a (Small).jpg")
    );
    std::fs::write(d.path().join("a (Small).jpg"), "").unwrap();
    std::fs::write(d.path().join("a (Small) (1).jpg"), "").unwrap();
    assert_eq!(
        unique_path(d.path(), "a (Small)", "jpg"),
        d.path().join("a (Small) (2).jpg")
    );
    // The extension follows the format, keeping the original's spelling.
    assert_eq!(
        output_extension(Path::new("/x/a.JPEG"), OutputFormat::Jpeg),
        "JPEG"
    );
    assert_eq!(
        output_extension(Path::new("/x/a.heic"), OutputFormat::Jpeg),
        "jpg"
    );
    assert_eq!(
        output_extension(Path::new("/x/a.png"), OutputFormat::Png),
        "png"
    );
    let opts = ResizeOptions::default();
    assert_eq!(
        destination_path(&d.path().join("b.png"), &opts, OutputFormat::Png, (1, 1)),
        d.path().join("b (Small).png")
    );
    let opts = ResizeOptions {
        destination: Some(d.path().join("out")),
        file_name_format: "%1-%5".into(),
        ..Default::default()
    };
    assert_eq!(
        destination_path(
            Path::new("/elsewhere/c.jpg"),
            &opts,
            OutputFormat::Jpeg,
            (640, 1)
        ),
        d.path().join("out/c-640.jpg")
    );
}

#[test]
fn formats_and_sizes() {
    assert_eq!(OutputFormat::parse("JPG"), Some(OutputFormat::Jpeg));
    assert_eq!(OutputFormat::parse("keep"), Some(OutputFormat::Keep));
    assert_eq!(
        OutputFormat::from_uti("public.heic"),
        Some(OutputFormat::Heic)
    );
    assert_eq!(
        resolve_format(OutputFormat::Png, Some(OutputFormat::Jpeg)),
        OutputFormat::Png
    );
    assert_eq!(resolve_format(OutputFormat::Keep, None), OutputFormat::Jpeg);
    assert_eq!(
        resolve_format(OutputFormat::Keep, Some(OutputFormat::Png)),
        OutputFormat::Png
    );
    assert_eq!(
        ResizeSize::default_presets()[2].describe(),
        "1920 × 1080 px"
    );
    assert_eq!(
        ResizeSize::new("h", ResizeFit::Fit, 50.0, 0.0, ResizeUnit::Percent).describe(),
        "50 %"
    );
    assert_eq!(px(0.0, 600.0, ResizeFit::Fit).describe(), "auto × 600 px");
    assert!(is_image_path(Path::new("a/B.HEIC")));
    assert!(!is_image_path(Path::new("a/b.txt")));
    let f = available_formats();
    assert_eq!(f[0], OutputFormat::Keep);
    #[cfg(target_os = "macos")]
    assert!(f.contains(&OutputFormat::Jpeg) && f.contains(&OutputFormat::Png));
}

#[test]
fn cli_parsing() {
    use super::cli::{parse, parse_size_spec};
    let cfg = crate::config::ResizeConfig::default();
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let c = parse(&s(&["a.jpg", "b.png"]), &cfg).unwrap();
    assert!(!c.direct);
    assert_eq!(c.paths.len(), 2);
    assert_eq!(c.opts.size.name, "Small");
    let c = parse(
        &s(&["--size", "large", "--format", "png", "-q", "70", "x.jpg"]),
        &cfg,
    )
    .unwrap();
    assert!(c.direct);
    assert_eq!(c.opts.size.width, 1920.0);
    assert_eq!(c.opts.format, OutputFormat::Png);
    assert_eq!(c.opts.jpeg_quality, 70);
    let c = parse(
        &s(&["--size", "800x0", "--fit", "fill", "--replace", "x"]),
        &cfg,
    )
    .unwrap();
    assert_eq!((c.opts.size.width, c.opts.size.height), (800.0, 0.0));
    assert_eq!(c.opts.size.fit, ResizeFit::Fill);
    assert!(c.opts.replace);
    let c = parse(
        &s(&["--size", "25%", "--strip-metadata", "--", "-odd.jpg"]),
        &cfg,
    )
    .unwrap();
    assert_eq!(c.opts.size.unit, ResizeUnit::Percent);
    assert!(!c.opts.keep_metadata);
    assert_eq!(c.paths, vec![PathBuf::from("-odd.jpg")]);
    assert!(parse(&s(&["--size", "huge", "x"]), &cfg).is_err());
    assert!(parse(&s(&["--bogus"]), &cfg).is_err());
    assert_eq!(
        parse_size_spec("10x5cm"),
        Some((10.0, 5.0, Some(ResizeUnit::Centimeter)))
    );
    assert_eq!(parse_size_spec("1024×768"), Some((1024.0, 768.0, None)));
    assert_eq!(parse_size_spec("x600"), Some((0.0, 600.0, None)));
    assert_eq!(parse_size_spec("abc"), None);
}

// ---------------------------------------------------------------------------
// Real resizes (ImageIO)

#[cfg(target_os = "macos")]
mod real {
    use super::super::mac::{center_pixel, exif_date, write_test_image};
    use super::*;

    fn sips_size(p: &Path) -> (u32, u32) {
        let out = std::process::Command::new("/usr/bin/sips")
            .args(["-g", "pixelWidth", "-g", "pixelHeight"])
            .arg(p)
            .output()
            .unwrap();
        let s = String::from_utf8_lossy(&out.stdout);
        let get = |k: &str| {
            s.lines()
                .find_map(|l| {
                    l.trim().strip_prefix(k).map(|v| {
                        v.trim()
                            .trim_start_matches(':')
                            .trim()
                            .parse::<u32>()
                            .unwrap()
                    })
                })
                .unwrap()
        };
        (get("pixelWidth"), get("pixelHeight"))
    }

    fn opts(size: ResizeSize) -> ResizeOptions {
        ResizeOptions {
            size,
            ..Default::default()
        }
    }

    #[test]
    fn jpeg_fit_keeps_metadata_and_gps_options() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("Beach.jpg");
        write_test_image(&src, 1200, 800, OutputFormat::Jpeg, 1, true, 300.0).unwrap();
        let info = image_info(&src).unwrap();
        assert_eq!((info.width, info.height), (1200, 800));
        assert_eq!(info.format, Some(OutputFormat::Jpeg));
        assert!(info.has_gps && info.has_exif);
        assert_eq!(info.dpi_x, 300.0);

        let small = ResizeSize::default_presets().remove(0);
        let out = resize_one(&src, &opts(small.clone())).unwrap();
        assert_eq!(out, d.path().join("Beach (Small).jpg"));
        assert_eq!(sips_size(&out), (720, 480));
        let i = image_info(&out).unwrap();
        assert_eq!(i.format, Some(OutputFormat::Jpeg));
        assert!(i.has_gps, "GPS kept by default");
        assert_eq!(exif_date(&out).as_deref(), Some("2024:07:01 12:34:56"));
        assert_eq!(i.dpi_x, 300.0);

        // Again: a unique name, GPS removed.
        let o = ResizeOptions {
            remove_gps: true,
            ..opts(small.clone())
        };
        let out2 = resize_one(&src, &o).unwrap();
        assert_eq!(out2, d.path().join("Beach (Small) (1).jpg"));
        let i = image_info(&out2).unwrap();
        assert!(!i.has_gps);
        assert_eq!(exif_date(&out2).as_deref(), Some("2024:07:01 12:34:56"));

        // No metadata at all.
        let o = ResizeOptions {
            keep_metadata: false,
            ..opts(small)
        };
        let out3 = resize_one(&src, &o).unwrap();
        let i = image_info(&out3).unwrap();
        assert!(!i.has_gps);
        assert_eq!(exif_date(&out3), None);
        // Colors survive (the center of the gradient).
        let c = center_pixel(&out3).unwrap();
        let c0 = center_pixel(&src).unwrap();
        for k in 0..3 {
            assert!(
                (i32::from(c[k]) - i32::from(c0[k])).abs() < 40,
                "{c:?} vs {c0:?}"
            );
        }
    }

    #[test]
    fn png_stretch_fill_and_formats() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("shot.png");
        write_test_image(&src, 400, 300, OutputFormat::Png, 1, false, 72.0).unwrap();

        let out = resize_one(&src, &opts(px(100.0, 100.0, ResizeFit::Stretch))).unwrap();
        assert_eq!(out.extension().unwrap(), "png");
        // Independent decoder check.
        let img = image::open(&out).unwrap();
        assert_eq!((img.width(), img.height()), (100, 100));

        let out = resize_one(&src, &opts(px(300.0, 200.0, ResizeFit::Fill))).unwrap();
        assert_eq!(sips_size(&out), (300, 200));
        assert_eq!(image_info(&out).unwrap().format, Some(OutputFormat::Png));

        // PNG → JPEG (with a white background), HEIC / WebP when writable.
        for f in available_formats().into_iter().skip(1) {
            let o = ResizeOptions {
                format: f,
                jpeg_quality: 60,
                file_name_format: format!("%1 %2 {}", f.label()),
                ..opts(px(200.0, 0.0, ResizeFit::Fit))
            };
            let out = resize_one(&src, &o).unwrap_or_else(|e| panic!("{f:?}: {e:#}"));
            assert_eq!(
                out.extension().unwrap().to_string_lossy(),
                f.extension().unwrap(),
                "{f:?}"
            );
            let i = image_info(&out).unwrap();
            assert_eq!((i.width, i.height), (200, 150), "{f:?}");
            assert_eq!(i.format, Some(f), "{f:?}");
        }
    }

    #[test]
    fn orientation_units_shrink_replace() {
        let d = tempfile::tempdir().unwrap();
        // Stored 800x600, EXIF orientation 6 (rotate 90°): shown 600x800.
        let src = d.path().join("rotated.jpg");
        write_test_image(&src, 800, 600, OutputFormat::Jpeg, 6, false, 72.0).unwrap();
        let info = image_info(&src).unwrap();
        assert_eq!((info.width, info.height, info.orientation), (600, 800, 6));
        let out = resize_one(&src, &opts(px(300.0, 300.0, ResizeFit::Fit))).unwrap();
        let i = image_info(&out).unwrap();
        assert_eq!((i.width, i.height, i.orientation), (225, 300, 1));
        assert_eq!(
            sips_size(&out),
            (225, 300),
            "pixels upright, not rotated by a tag"
        );

        // Percent.
        let pct = ResizeSize::new("Quarter", ResizeFit::Fit, 25.0, 0.0, ResizeUnit::Percent);
        let out = resize_one(&src, &opts(pct)).unwrap();
        assert_eq!(out, d.path().join("rotated (Quarter).jpg"));
        assert_eq!(sips_size(&out), (150, 200));

        // Inches at 72 dpi.
        let inch = ResizeSize::new("In", ResizeFit::Stretch, 2.0, 1.0, ResizeUnit::Inch);
        let o = ResizeOptions {
            ignore_orientation: false,
            ..opts(inch)
        };
        assert_eq!(sips_size(&resize_one(&src, &o).unwrap()), (144, 72));

        // Only shrink: a copy at the original size.
        let o = ResizeOptions {
            shrink_only: true,
            ..opts(ResizeSize::default_presets().remove(2))
        };
        let out = resize_one(&src, &o).unwrap();
        assert_eq!(sips_size(&out), (600, 800));

        // Replace + keep date modified.
        let victim = d.path().join("victim.jpg");
        std::fs::copy(&src, &victim).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        std::fs::File::options()
            .write(true)
            .open(&victim)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let o = ResizeOptions {
            replace: true,
            keep_date_modified: true,
            ..opts(px(100.0, 100.0, ResizeFit::Fit))
        };
        let out = resize_one(&victim, &o).unwrap();
        assert_eq!(out, victim);
        assert_eq!(sips_size(&victim), (75, 100));
        assert_eq!(std::fs::metadata(&victim).unwrap().modified().unwrap(), old);
        // Replace with another format: new extension, the original is gone.
        let o = ResizeOptions {
            replace: true,
            format: OutputFormat::Png,
            ..opts(px(50.0, 50.0, ResizeFit::Fit))
        };
        let out = resize_one(&victim, &o).unwrap();
        assert_eq!(out, d.path().join("victim.png"));
        assert!(!victim.exists());
        // No temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("pm-resize"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn batch_reports_failures() {
        let d = tempfile::tempdir().unwrap();
        let good = d.path().join("good.png");
        write_test_image(&good, 64, 64, OutputFormat::Png, 1, false, 72.0).unwrap();
        let bad = d.path().join("notes.txt");
        std::fs::write(&bad, "hello").unwrap();
        let fake = d.path().join("fake.jpg");
        std::fs::write(&fake, "not a jpeg").unwrap();
        let files = collect_images(&[d.path().to_path_buf(), bad.clone()]);
        assert_eq!(files, vec![fake.clone(), good.clone(), bad.clone()]);
        let mut seen = Vec::new();
        let r = resize_with_progress(
            &files,
            &opts(px(32.0, 32.0, ResizeFit::Fit)),
            &mut |i, r| seen.push((i, r.is_ok())),
        );
        assert_eq!(seen, vec![(0, false), (1, true), (2, false)]);
        assert!(r[1].as_ref().unwrap().ends_with("good (Test).png"));
        let r = resize(
            &[d.path().to_path_buf()],
            &opts(px(32.0, 32.0, ResizeFit::Fit)),
        );
        assert!(r[0].is_err());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn cli_resizes_directly() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("cli.png");
    super::mac::write_test_image(&src, 200, 100, OutputFormat::Png, 1, false, 72.0).unwrap();
    let cfg = d.path().join("ft.json");
    let args: Vec<String> = ["--size", "50%", "--format", "jpg", "-q", "75"]
        .iter()
        .map(|s| s.to_string())
        .chain([src.display().to_string()])
        .collect();
    assert_eq!(super::cli::run_with(&args, Some(&cfg)).unwrap(), 0);
    let out = d.path().join("cli (Custom).jpg");
    let i = image_info(&out).unwrap();
    assert_eq!(
        (i.width, i.height, i.format),
        (100, 50, Some(OutputFormat::Jpeg))
    );
    // A failing file gives exit code 1.
    let bad = d.path().join("bad.jpg");
    std::fs::write(&bad, "no").unwrap();
    let args = vec!["-y".to_string(), bad.display().to_string()];
    assert_eq!(super::cli::run_with(&args, Some(&cfg)).unwrap(), 1);
}
