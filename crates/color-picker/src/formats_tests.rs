//! Test vectors taken from PowerToys' ColorPickerUI.UnitTests
//! (ColorConverterTest, ColorFormatConversionTest, ColorRepresentationHelperTest).

use super::formats::*;

fn hex(s: &str) -> Rgb {
    Rgb::from_hex(s).unwrap()
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn hsl_table() {
    // (h, s%, l%, r%, g%, b%) from ColorRGBtoHSLTest
    let rows: &[(f64, f64, f64, f64, f64, f64)] = &[
        (0., 0., 0., 0., 0., 0.),
        (0., 0., 100., 100., 100., 100.),
        (0., 100., 50., 100., 0., 0.),
        (15., 100., 50., 100., 25., 0.),
        (20., 60., 22.5, 36., 18., 9.),
        (30., 100., 50., 100., 50., 0.),
        (60., 100., 50., 100., 100., 0.),
        (105., 100., 50., 25., 100., 0.),
        (120., 100., 25., 0., 50., 0.),
        (180., 100., 50., 0., 100., 100.),
        (240., 100., 50., 0., 0., 100.),
        (270., 100., 50., 50., 0., 100.),
        (345., 100., 50., 100., 0., 25.),
    ];
    for &(h, s, l, r, g, b) in rows {
        let to = |p: f64| (p / 100.0 * 255.0).round() as u8;
        let c = Rgb::new(to(r), to(g), to(b));
        let (rh, rs, rl) = to_hsl(c);
        // PowerToys: hue ±0.5° (rounded percent inputs: ±1.5°), s/l ±0.5%.
        assert!(close(rh, h, 1.5), "{c} hue {rh} != {h}");
        assert!(close(rs * 100.0, s, 2.0), "{c} sat {} != {s}", rs * 100.0);
        assert!(close(rl * 100.0, l, 0.5), "{c} light {} != {l}", rl * 100.0);
    }
}

#[test]
fn hsv_table() {
    let rows: &[(f64, f64, f64, f64, f64, f64)] = &[
        (0., 0., 0., 0., 0., 0.),
        (0., 0., 100., 100., 100., 100.),
        (0., 100., 100., 100., 0., 0.),
        (20., 75., 36., 36., 18., 9.),
        (120., 100., 50., 0., 50., 0.),
        (195., 100., 100., 0., 75., 100.),
        (300., 100., 100., 100., 0., 100.),
    ];
    for &(h, s, v, r, g, b) in rows {
        let to = |p: f64| (p / 100.0 * 255.0).round() as u8;
        let c = Rgb::new(to(r), to(g), to(b));
        let (rh, rs, rv) = to_hsv(c);
        assert!(close(rh, h, 1.5), "{c} hue {rh} != {h}");
        assert!(close(rs * 100.0, s, 1.0), "{c} sat");
        assert!(close(rv * 100.0, v, 0.5), "{c} value");
    }
}

#[test]
fn cmyk_table() {
    // (c, m, y, k, r, g, b) from ColorRGBtoCMYKTest
    let rows: &[(f64, f64, f64, f64, u8, u8, u8)] = &[
        (0., 0., 0., 100., 0, 0, 0),
        (0., 0., 0., 0., 255, 255, 255),
        (0., 100., 100., 0., 255, 0, 0),
        (0., 75., 100., 0., 255, 64, 0),
        (0., 50., 75., 64., 92, 46, 23),
        (0., 50., 100., 0., 255, 128, 0),
        (25., 0., 100., 0., 192, 255, 0),
        (100., 0., 100., 50., 0, 128, 0),
        (100., 25., 0., 0., 0, 192, 255),
        (75., 100., 0., 0., 64, 0, 255),
        (0., 100., 75., 0., 255, 0, 64),
    ];
    for &(cy, ma, ye, k, r, g, b) in rows {
        let (a, bb, c, d) = to_cmyk(Rgb::new(r, g, b));
        assert!(
            close(a * 100.0, cy, 0.5)
                && close(bb * 100.0, ma, 0.5)
                && close(c * 100.0, ye, 0.5)
                && close(d * 100.0, k, 0.5),
            "cmyk of {r},{g},{b}: {a} {bb} {c} {d}"
        );
    }
}

#[test]
fn hsi_table() {
    let rows: &[(&str, f64, f64, f64)] = &[
        ("FFFFFF", 0.0, 0.0, 100.0),
        ("808080", 0.0, 0.0, 50.0),
        ("000000", 0.0, 0.0, 0.0),
        ("FF0000", 0.0, 100.0, 33.3),
        ("BFBF00", 60.0, 100.0, 50.0),
        ("008000", 120.0, 100.0, 16.7),
        ("80FFFF", 180.0, 40.0, 83.3),
        ("8080FF", 240.0, 25.0, 66.7),
        ("BF40BF", 300.0, 57.1, 58.3),
        ("A0A424", 61.8, 69.9, 47.1),
        ("411BEA", 251.1, 75.6, 42.6),
        ("1EAC41", 134.9, 66.7, 34.9),
        ("F0C80E", 49.5, 91.1, 59.3),
        ("B430E5", 283.7, 68.6, 59.6),
        ("ED7651", 14.3, 44.6, 57.0),
        ("FEF888", 56.9, 36.3, 83.5),
        ("19CB97", 162.4, 80.0, 49.5),
        ("362698", 248.3, 53.3, 31.9),
        ("7E7EB8", 240.5, 13.5, 57.0),
    ];
    for &(h, hu, s, i) in rows {
        let (rh, rs, ri) = to_hsi(hex(h));
        assert!(
            close(rh, hu, 0.5) && close(rs * 100.0, s, 0.5) && close(ri * 100.0, i, 0.5),
            "hsi {h}: {rh} {rs} {ri}"
        );
    }
}

#[test]
fn hwb_and_ncol_table() {
    let rows: &[(&str, f64, &str, f64, f64)] = &[
        ("FFFFFF", 0.0, "R0", 100.0, 0.0),
        ("808080", 0.0, "R0", 50.0, 50.0),
        ("000000", 0.0, "R0", 0.0, 100.0),
        ("FF0000", 0.0, "R0", 0.0, 0.0),
        ("BFBF00", 60.0, "Y0", 0.0, 25.0),
        ("008000", 120.0, "G0", 0.0, 50.0),
        ("80FFFF", 180.0, "C0", 50.0, 0.0),
        ("8080FF", 240.0, "B0", 50.0, 0.0),
        ("BF40BF", 300.0, "M0", 25.0, 25.0),
        ("A0A424", 62.0, "Y3", 14.0, 36.0),
        ("411BEA", 251.0, "B18", 11.0, 8.0),
        ("1EAC41", 135.0, "G25", 12.0, 33.0),
        ("F0C80E", 49.0, "R82", 5.0, 6.0),
        ("B430E5", 284.0, "B73", 19.0, 10.0),
        ("ED7651", 14.0, "R24", 32.0, 7.0),
        ("FEF888", 57.0, "R95", 53.0, 0.0),
        ("19CB97", 162.0, "G71", 10.0, 20.0),
        ("362698", 248.0, "B14", 15.0, 40.0),
        ("7E7EB8", 240.0, "B0", 49.0, 28.0),
    ];
    for &(h, hu, ncol, w, b) in rows {
        let (rh, rw, rb) = to_hwb(hex(h));
        assert!(
            close(rh, hu, 0.5) && close(rw * 100.0, w, 0.5) && close(rb * 100.0, b, 0.5),
            "hwb {h}"
        );
        let (nh, nw, nb) = to_ncol(hex(h));
        assert_eq!(nh, ncol, "ncol {h}");
        assert!(close(nw * 100.0, w, 0.5) && close(nb * 100.0, b, 0.5));
    }
}

#[test]
fn cielab_table() {
    let rows: &[(&str, f64, f64, f64)] = &[
        ("FFFFFF", 100.00, 0.00, 0.00),
        ("808080", 53.59, 0.00, 0.00),
        ("000000", 0.00, 0.00, 0.00),
        ("FF0000", 53.24, 80.09, 67.20),
        ("008000", 46.23, -51.70, 49.90),
        ("80FFFF", 93.16, -35.23, -10.87),
        ("8080FF", 59.20, 33.10, -63.46),
        ("BF40BF", 50.10, 65.50, -41.48),
        ("BFBF00", 75.04, -17.35, 76.03),
        ("0048BA", 34.35, 27.94, -64.80),
        ("B0BF1A", 73.91, -23.39, 71.15),
        ("D0FF14", 93.87, -40.20, 88.97),
        ("1B4D3E", 29.13, -20.96, 3.95),
        ("FFEF00", 93.01, -13.86, 91.48),
        ("FFA600", 75.16, 23.41, 79.10),
        ("1A2421", 13.18, -5.23, 0.56),
        ("003399", 25.76, 28.89, -59.09),
        ("D70A53", 46.03, 71.90, 18.03),
        ("80FFD5", 92.09, -45.08, 9.29),
        ("EFDFBB", 89.26, -0.13, 19.65),
        ("5218FA", 36.65, 75.63, -97.70),
        ("FF496C", 59.08, 69.89, 21.80),
        ("545AA7", 41.20, 19.32, -42.34),
        ("E6A8D7", 75.91, 30.13, -14.79),
        ("ADDFAD", 84.32, -25.67, 19.37),
        ("E3F988", 94.25, -23.70, 51.58),
    ];
    let r2 = |x: f64| (x * 100.0).round_ties_even() / 100.0;
    for &(h, l, a, b) in rows {
        let (rl, ra, rb) = to_lab(hex(h));
        assert_eq!(
            (r2(rl) + 0.0, r2(ra) + 0.0, r2(rb) + 0.0),
            (l, a, b),
            "lab {h}"
        );
    }
}

#[test]
fn oklab_oklch_table() {
    let rows: &[(&str, f64, f64, f64, f64, f64)] = &[
        // hex, L, a, b, chroma, hue
        ("FFFFFF", 1.00, 0.00, 0.00, 0.0, 0.0),
        ("808080", 0.6, 0.00, 0.00, 0.0, 0.0),
        ("000000", 0.00, 0.00, 0.00, 0.0, 0.0),
        ("FF0000", 0.628, 0.22, 0.13, 0.258, 29.23),
        ("008000", 0.52, -0.14, 0.11, 0.177, 142.5),
        ("80FFFF", 0.928, -0.11, -0.03, 0.113, 195.38),
        ("8080FF", 0.661, 0.03, -0.18, 0.184, 280.13),
        ("BF40BF", 0.598, 0.18, -0.11, 0.216, 327.86),
        ("BFBF00", 0.779, -0.06, 0.16, 0.17, 109.77),
        ("0048BA", 0.444, -0.03, -0.19, 0.19, 260.86),
        ("D70A53", 0.563, 0.22, 0.04, 0.222, 11.5),
        ("EFDFBB", 0.907, 0.0, 0.05, 0.05, 86.89),
        ("5218FA", 0.489, 0.05, -0.28, 0.286, 279.13),
        ("E3F988", 0.942, -0.07, 0.12, 0.141, 118.24),
    ];
    let r = |x: f64, d: i32| {
        let p = 10f64.powi(d);
        (x * p).round_ties_even() / p + 0.0
    };
    for &(h, l, a, b, c, hu) in rows {
        let (ol, oa, ob) = to_oklab(hex(h));
        assert_eq!((r(ol, 3), r(oa, 2), r(ob, 2)), (l, a, b), "oklab {h}");
        let (_, cc, hh) = to_oklch(hex(h));
        assert_eq!((r(cc, 3), r(hh, 2)), (c, hu), "oklch {h}");
    }
}

#[test]
fn xyz_table() {
    let rows: &[(&str, f64, f64, f64)] = &[
        ("FFFFFF", 95.0456, 100.0000, 108.9058),
        ("808080", 20.5166, 21.5861, 23.5085),
        ("000000", 0.0, 0.0, 0.0),
        ("FF0000", 41.2391, 21.2639, 1.9331),
        ("008000", 7.7188, 15.4377, 2.5729),
        ("80FFFF", 62.7084, 83.3261, 107.3900),
        ("8080FF", 34.6688, 27.2469, 98.0434),
        ("BF40BF", 32.7217, 18.5062, 51.1405),
        ("BFBF00", 40.1154, 48.3384, 7.2171),
        ("0048BA", 11.1792, 8.1793, 47.4455),
        ("B0BF1A", 36.7205, 46.5663, 8.0311),
        ("D0FF14", 61.8965, 84.9797, 13.8037),
        ("1B4D3E", 3.9752, 5.8883, 5.4847),
        ("FFEF00", 72.1042, 82.9942, 12.2215),
        ("FFA600", 54.8747, 48.5351, 6.4783),
        ("1A2421", 1.3313, 1.5911, 1.6759),
        ("003399", 6.9329, 4.6672, 30.6735),
        ("D70A53", 29.6934, 15.2913, 9.5719),
        ("80FFD5", 56.6693, 80.9105, 75.5840),
        ("EFDFBB", 70.9510, 74.7146, 57.6991),
        ("5218FA", 21.0597, 9.3488, 91.1403),
        ("FF496C", 46.3280, 27.1114, 16.9814),
        ("545AA7", 14.2864, 11.9869, 38.1214),
        ("E6A8D7", 58.8989, 49.7359, 70.7897),
        ("ADDFAD", 51.1617, 64.6757, 49.3246),
        ("E3F988", 69.9955, 85.8597, 36.1785),
    ];
    let r4 = |x: f64| (x * 100.0 * 10000.0).round_ties_even() / 10000.0;
    for &(h, x, y, z) in rows {
        let (rx, ry, rz) = to_xyz(hex(h));
        assert_eq!((r4(rx), r4(ry), r4(rz)), (x, y, z), "xyz {h}");
    }
}

#[test]
fn black_in_every_builtin_format() {
    // ColorRepresentationHelperTest
    let expected = [
        ("CMYK", "cmyk(0%, 0%, 0%, 100%)"),
        ("HEX", "000000"),
        ("NCol", "R0, 0%, 100%"),
        ("HSB", "hsb(0, 0%, 0%)"),
        ("HSI", "hsi(0, 0%, 0%)"),
        ("HSL", "hsl(0, 0%, 0%)"),
        ("HSV", "hsv(0, 0%, 0%)"),
        ("HWB", "hwb(0, 0%, 100%)"),
        ("RGB", "rgb(0, 0, 0)"),
        ("CIEXYZ", "XYZ(0, 0, 0)"),
        ("CIELAB", "CIELab(0, 0, 0)"),
        ("Oklab", "oklab(0, 0, 0)"),
        ("Oklch", "oklch(0, 0, 0)"),
        ("VEC4", "(0f, 0f, 0f, 1f)"),
        ("Decimal", "0"),
        ("HEX Int", "0xFF000000"),
        ("SwiftUI", "Color(red: 0, green: 0, blue: 0)"),
        (
            "NSColor",
            "NSColor(srgbRed: 0, green: 0, blue: 0, alpha: 1)",
        ),
    ];
    for (name, want) in expected {
        assert_eq!(
            format_color(Rgb::new(0, 0, 0), builtin_format(name).unwrap()),
            want,
            "{name}"
        );
    }
}

#[test]
fn moccasin_in_every_builtin_format() {
    // PowerToys' example color (Color.Moccasin = #FFE4B5), values computed with
    // the PowerToys algorithms.
    let c = hex("FFE4B5");
    let want = [
        ("HEX", "ffe4b5"),
        ("RGB", "rgb(255, 228, 181)"),
        ("HSL", "hsl(38, 100%, 85%)"),
        ("HSV", "hsv(38, 29%, 100%)"),
        ("CMYK", "cmyk(0%, 11%, 29%, 0%)"),
        ("HSB", "hsb(38, 29%, 100%)"),
        ("HSI", "hsi(38, 18%, 87%)"),
        ("HWB", "hwb(38, 71%, 0%)"),
        ("NCol", "R64, 71%, 0%"),
        ("CIEXYZ", "XYZ(77.3209, 80.0841, 55.1024)"),
        ("CIELAB", "CIELab(91.72, 2.44, 26.36)"),
        ("Oklab", "oklab(0.93, 0.01, 0.07)"),
        ("Oklch", "oklch(0.93, 0.07, 81.38)"),
        ("VEC4", "(1f, 0.89f, 0.71f, 1f)"),
        ("Decimal", "11920639"),
        ("HEX Int", "0xFFFFE4B5"),
        ("SwiftUI", "Color(red: 1, green: 0.89, blue: 0.71)"),
        (
            "UIColor",
            "UIColor(red: 1, green: 0.89, blue: 0.71, alpha: 1)",
        ),
        (
            "Swift literal",
            "#colorLiteral(red: 1, green: 0.89, blue: 0.71, alpha: 1)",
        ),
    ];
    for (name, w) in want {
        assert_eq!(format_color(c, builtin_format(name).unwrap()), w, "{name}");
    }
}

#[test]
fn template_formatters() {
    let c = Rgb::new(0xAB, 0x05, 0xFF);
    assert_eq!(format_color(c, "%Reh%Grh%Blh"), "a0f");
    assert_eq!(format_color(c, "%ReH%GrH%BlH"), "A0F");
    assert_eq!(format_color(c, "#%ReX%GrX%BlX"), "#AB05FF");
    assert_eq!(format_color(c, "%Reb/%Grb/%Blb/%Al"), "171/5/255/255");
    assert_eq!(format_color(c, "%ReF %GrF %BlF"), ".67 .02 1");
    assert_eq!(format_color(Rgb::new(0, 0, 0), "[%ReF]"), "[]");
    assert_eq!(format_color(c, "%Dr"), (0xAB05FFu32).to_string());
    assert_eq!(format_color(c, "%Dv"), (0xFF05ABu32).to_string());
    // Unknown parameters and a trailing % are left alone.
    assert_eq!(format_color(c, "100% %Zz %"), "100% %Zz %");
    assert_eq!(format_color(c, "%Re"), "171");
    assert_eq!(format_color(c, "x%Re%"), "x171%");
    // CIELAB integer formatter.
    assert_eq!(format_color(hex("FF0000"), "%Lci %Cai %Cbi"), "53 80 67");
    // Color name.
    assert_eq!(format_color(hex("FFFFFF"), "%Na"), "White");
    // Empty template is lower-case hex.
    assert_eq!(format_color(c, ""), "ab05ff");
}

#[test]
fn color_names() {
    assert_eq!(color_name(hex("000000")), "Black");
    assert_eq!(color_name(hex("FFFFFF")), "White");
    assert_eq!(color_name(hex("808080")), "Gray");
    assert_eq!(color_name(hex("C0C0C0")), "Light gray");
    assert_eq!(color_name(hex("404040")), "Dark gray");
    assert_eq!(color_name(hex("FF0000")), "Red");
    assert_eq!(color_name(hex("0000FF")), "Blue");
    assert_eq!(color_name(hex("00FF00")), "Green");
    assert_eq!(color_name(hex("FFFF00")), "Yellow");
    assert_eq!(color_name(hex("800000")), "Dark red");
    assert_eq!(color_name(hex("ADD8E6")), "Sky blue");
}

#[test]
fn hex_parsing() {
    assert_eq!(Rgb::from_hex("#ffe4b5"), Some(Rgb::new(255, 228, 181)));
    assert_eq!(Rgb::from_hex("FFE4B5"), Some(Rgb::new(255, 228, 181)));
    assert_eq!(Rgb::from_hex("#fff"), Some(Rgb::new(255, 255, 255)));
    assert_eq!(Rgb::from_hex("0xFFFFE4B5"), Some(Rgb::new(255, 228, 181)));
    assert_eq!(Rgb::from_hex("#ggg"), None);
    assert_eq!(Rgb::from_hex("#12345"), None);
    assert_eq!(Rgb::new(1, 2, 255).hex(), "#0102FF");
    let json = serde_json::to_string(&vec![Rgb::new(1, 2, 3)]).unwrap();
    assert_eq!(json, r##"["#010203"]"##);
    let back: Vec<Rgb> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, vec![Rgb::new(1, 2, 3)]);
}

#[test]
fn hsv_round_trip_and_variations() {
    for c in [
        hex("FFE4B5"),
        hex("123456"),
        hex("FF0000"),
        hex("00FF80"),
        hex("808080"),
    ] {
        let (h, s, v) = to_hsv(c);
        assert_eq!(from_hsv(h, s, v), c);
    }
    // Pure red: v = 1, so lighter variants shift the hue; darker ones are 80% / 70%.
    let [l2, l1, d1, d2] = variations(hex("FF0000"));
    assert_eq!(l2, from_hsv_truncating(8.0, 1.0, 1.0));
    assert_eq!(l1, from_hsv_truncating(4.0, 1.0, 1.0));
    assert_eq!(d1, Rgb::new(204, 0, 0));
    assert_eq!(d2, Rgb::new(178, 0, 0));
    // A mid gray gets lighter / darker grays.
    let [a, b, c, d] = variations(hex("808080"));
    assert!(a.r > b.r && b.r > 0x80 && c.r < 0x80 && d.r < c.r);
    assert_eq!(mix_white_black(hex("000000"), 0.5), Rgb::new(128, 128, 128));
    assert_eq!(
        mix_white_black(hex("FFFFFF"), -0.5),
        Rgb::new(128, 128, 128)
    );
}
