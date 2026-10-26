//! OCR on images rendered here with CoreText (no screen capture or
//! permission needed).

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSFont, NSFontAttributeName};
use objc2_foundation::{
    NSAttributedString, NSDictionary, NSPoint, NSRect, NSSize, NSString, NSURL,
};

use super::cg::CgImage;
use crate::Config;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(cs: *mut c_void);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        w: usize,
        h: usize,
        bpc: usize,
        bpr: usize,
        cs: *mut c_void,
        info: u32,
    ) -> *mut c_void;
    fn CGContextSetRGBFillColor(ctx: *mut c_void, r: f64, g: f64, b: f64, a: f64);
    fn CGContextFillRect(ctx: *mut c_void, rect: NSRect);
    fn CGContextSetTextPosition(ctx: *mut c_void, x: f64, y: f64);
    fn CGBitmapContextCreateImage(ctx: *mut c_void) -> *const c_void;
    fn CGContextRelease(ctx: *mut c_void);
}

#[link(name = "CoreText", kind = "framework")]
extern "C" {
    fn CTLineCreateWithAttributedString(s: *const c_void) -> *const c_void;
    fn CTLineDraw(line: *const c_void, ctx: *mut c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(p: *const c_void);
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageDestinationCreateWithURL(
        url: *const c_void,
        ty: *const c_void,
        count: usize,
        opts: *const c_void,
    ) -> *mut c_void;
    fn CGImageDestinationAddImage(dest: *mut c_void, img: *const c_void, props: *const c_void);
    fn CGImageDestinationFinalize(dest: *mut c_void) -> bool;
}

/// Render `lines` (black on white, `size` pt) into an image; also returns
/// the RGBA pixels.
fn render(lines: &[&str], size: f64, w: usize, h: usize) -> (CgImage, Vec<u8>) {
    let mut px = vec![0u8; w * h * 4];
    unsafe {
        let cs = CGColorSpaceCreateDeviceRGB();
        // kCGImageAlphaPremultipliedLast
        let ctx = CGBitmapContextCreate(px.as_mut_ptr() as *mut c_void, w, h, 8, w * 4, cs, 1);
        CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
        CGContextFillRect(
            ctx,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64)),
        );
        let font = NSFont::systemFontOfSize(size);
        let key: &NSString = NSFontAttributeName;
        let font_obj: &AnyObject = &font;
        let attrs = NSDictionary::from_slices(&[key], &[font_obj]);
        for (i, line) in lines.iter().enumerate() {
            let s: Retained<NSAttributedString> =
                NSAttributedString::new_with_attributes(&NSString::from_str(line), &attrs);
            let ct = CTLineCreateWithAttributedString(Retained::as_ptr(&s) as *const c_void);
            CGContextSetTextPosition(ctx, 30.0, h as f64 - (i as f64 + 1.0) * size * 1.6);
            CTLineDraw(ct, ctx);
            CFRelease(ct);
        }
        let img = CgImage::from_owned(CGBitmapContextCreateImage(ctx)).expect("image");
        CGContextRelease(ctx);
        CGColorSpaceRelease(cs);
        (img, px)
    }
}

fn norm(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

const LINES: [&str; 3] = [
    "The quick brown fox jumps over the lazy dog",
    "PowerMac Text Extractor 2026",
    "Copied to the clipboard.",
];

#[test]
fn recognizes_rendered_text() {
    let (img, _) = render(&LINES, 34.0, 1100, 220);
    assert_eq!(img.size(), (1100, 220));
    let frags = super::ocr_cgimage(&img, &Config::default()).expect("vision");
    let text = crate::text::assemble(frags, false);
    eprintln!("recognized:\n{text}");
    let got: Vec<String> = text.lines().map(norm).collect();
    let want: Vec<String> = LINES.iter().map(|l| norm(l)).collect();
    assert_eq!(got, want);
}

#[test]
fn single_line_and_languages() {
    let (img, _) = render(&LINES[..2], 34.0, 1100, 160);
    let cfg = Config {
        single_line: true,
        languages: vec!["en-US".into()],
        ..Default::default()
    };
    let text = crate::text::assemble(super::ocr_cgimage(&img, &cfg).unwrap(), true);
    assert_eq!(norm(&text), norm(&format!("{} {}", LINES[0], LINES[1])));
    assert!(!text.contains('\n'));
    let langs = super::supported_languages();
    assert!(langs.iter().any(|l| l.starts_with("en")), "{langs:?}");
}

#[test]
fn rgba_buffer_and_png_file() {
    let (img, px) = render(&["Hello from a PNG file", "second line 42"], 30.0, 700, 140);
    let text = crate::ocr_rgba(700, 140, &px, &Config::default()).unwrap();
    assert_eq!(
        text.lines().map(norm).collect::<Vec<_>>(),
        vec!["hello from a png file", "second line 42"]
    );
    // Write a PNG with ImageIO and read it back through ocr_image().
    let dir = std::env::temp_dir().join(format!("te-ocr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("text.png");
    unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let ty = NSString::from_str("public.png");
        let dest = CGImageDestinationCreateWithURL(
            Retained::as_ptr(&url) as *const c_void,
            Retained::as_ptr(&ty) as *const c_void,
            1,
            std::ptr::null(),
        );
        assert!(!dest.is_null());
        CGImageDestinationAddImage(dest, img.as_ref().0, std::ptr::null());
        assert!(CGImageDestinationFinalize(dest));
        CFRelease(dest);
    }
    let text = crate::ocr_image(&path).unwrap();
    assert_eq!(
        text.lines().map(norm).collect::<Vec<_>>(),
        vec!["hello from a png file", "second line 42"]
    );
    assert!(crate::ocr_image(dir.join("missing.png")).is_err());
    std::fs::write(dir.join("not-an-image.png"), b"nope").unwrap();
    assert!(crate::ocr_image(dir.join("not-an-image.png")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn transparent_rgba_is_flattened_on_white() {
    // Black text on a fully transparent background must still be readable.
    let (_, px) = render(&["Transparent background"], 34.0, 600, 90);
    let mut rgba = px.clone();
    for p in rgba.as_chunks_mut::<4>().0 {
        // white → transparent black, black → opaque black
        let a = 255 - p[0];
        p.copy_from_slice(&[0, 0, 0, a]);
    }
    let text = crate::ocr_rgba(600, 90, &rgba, &Config::default()).unwrap();
    assert_eq!(norm(&text), "transparent background");
}
