//! ImageIO / CoreGraphics backend of the Image Resizer (raw FFI).
//!
//! Decoding uses `CGImageSourceCreateThumbnailAtIndex` with
//! `kCGImageSourceThumbnailMaxPixelSize` (subsampled JPEG / HEIC decoding,
//! EXIF orientation applied), then a high-quality CoreGraphics draw to the
//! exact size (and the centered crop for "fill"). Writing uses
//! `CGImageDestination` with the lossy quality and the original's metadata
//! (orientation reset to 1, pixel dimensions updated, GPS optionally
//! removed).

#![allow(non_upper_case_globals, non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::{c_char, c_void};
use std::path::Path;

use anyhow::{bail, Context as _};

use super::{Dimensions, EncodeJob, ImageInfo, OutputFormat};

type CFTypeRef = *const c_void;
type CFIndex = isize;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize {
            width: w,
            height: h,
        },
    }
}

/// Opaque callback structs; only their addresses are used.
#[repr(C)]
struct CallBacks([usize; 6]);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: CallBacks;
    static kCFTypeDictionaryValueCallBacks: CallBacks;
    static kCFBooleanTrue: CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFURLCreateFromFileSystemRepresentation(
        alloc: CFTypeRef,
        buf: *const u8,
        len: CFIndex,
        is_dir: u8,
    ) -> CFTypeRef;
    fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFDictionaryCreateMutable(
        alloc: CFTypeRef,
        capacity: CFIndex,
        kcb: *const CallBacks,
        vcb: *const CallBacks,
    ) -> CFTypeRef;
    fn CFDictionaryCreateMutableCopy(
        alloc: CFTypeRef,
        capacity: CFIndex,
        d: CFTypeRef,
    ) -> CFTypeRef;
    fn CFDictionarySetValue(d: CFTypeRef, k: CFTypeRef, v: CFTypeRef);
    fn CFDictionaryRemoveValue(d: CFTypeRef, k: CFTypeRef);
    fn CFNumberCreate(alloc: CFTypeRef, ty: CFIndex, value: *const c_void) -> CFTypeRef;
    fn CFNumberGetValue(n: CFTypeRef, ty: CFIndex, out: *mut c_void) -> u8;
    fn CFStringCreateWithBytes(
        alloc: CFTypeRef,
        bytes: *const u8,
        len: CFIndex,
        encoding: u32,
        external: u8,
    ) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: CFIndex, enc: u32) -> u8;
    fn CFArrayGetCount(a: CFTypeRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: CFIndex) -> CFTypeRef;
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    static kCGImageSourceCreateThumbnailFromImageAlways: CFTypeRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFTypeRef;
    static kCGImageSourceCreateThumbnailWithTransform: CFTypeRef;
    static kCGImageSourceShouldCacheImmediately: CFTypeRef;
    static kCGImagePropertyPixelWidth: CFTypeRef;
    static kCGImagePropertyPixelHeight: CFTypeRef;
    static kCGImagePropertyDPIWidth: CFTypeRef;
    static kCGImagePropertyDPIHeight: CFTypeRef;
    static kCGImagePropertyOrientation: CFTypeRef;
    static kCGImagePropertyTIFFDictionary: CFTypeRef;
    static kCGImagePropertyTIFFOrientation: CFTypeRef;
    static kCGImagePropertyExifDictionary: CFTypeRef;
    static kCGImagePropertyExifPixelXDimension: CFTypeRef;
    static kCGImagePropertyExifPixelYDimension: CFTypeRef;
    static kCGImagePropertyGPSDictionary: CFTypeRef;
    #[cfg(test)]
    static kCGImagePropertyGPSLatitude: CFTypeRef;
    #[cfg(test)]
    static kCGImagePropertyGPSLatitudeRef: CFTypeRef;
    #[cfg(test)]
    static kCGImagePropertyExifDateTimeOriginal: CFTypeRef;
    static kCGImageDestinationLossyCompressionQuality: CFTypeRef;
    fn CGImageSourceCreateWithURL(url: CFTypeRef, opts: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceGetType(src: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceGetCount(src: CFTypeRef) -> usize;
    fn CGImageSourceCopyPropertiesAtIndex(src: CFTypeRef, i: usize, opts: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCreateThumbnailAtIndex(src: CFTypeRef, i: usize, opts: CFTypeRef) -> CFTypeRef;
    fn CGImageDestinationCreateWithURL(
        url: CFTypeRef,
        ty: CFTypeRef,
        count: usize,
        opts: CFTypeRef,
    ) -> CFTypeRef;
    fn CGImageDestinationAddImage(dest: CFTypeRef, image: CFTypeRef, props: CFTypeRef);
    fn CGImageDestinationFinalize(dest: CFTypeRef) -> bool;
    fn CGImageDestinationCopyTypeIdentifiers() -> CFTypeRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static kCGColorSpaceSRGB: CFTypeRef;
    fn CGImageGetWidth(img: CFTypeRef) -> usize;
    fn CGImageGetHeight(img: CFTypeRef) -> usize;
    fn CGImageGetColorSpace(img: CFTypeRef) -> CFTypeRef;
    fn CGImageGetAlphaInfo(img: CFTypeRef) -> u32;
    fn CGColorSpaceGetModel(cs: CFTypeRef) -> i32;
    fn CGColorSpaceCreateWithName(name: CFTypeRef) -> CFTypeRef;
    fn CGBitmapContextCreate(
        data: *mut c_void,
        w: usize,
        h: usize,
        bpc: usize,
        bpr: usize,
        cs: CFTypeRef,
        info: u32,
    ) -> CFTypeRef;
    fn CGContextSetInterpolationQuality(ctx: CFTypeRef, q: i32);
    fn CGContextDrawImage(ctx: CFTypeRef, r: CGRect, img: CFTypeRef);
    fn CGContextSetRGBFillColor(ctx: CFTypeRef, r: f64, g: f64, b: f64, a: f64);
    fn CGContextFillRect(ctx: CFTypeRef, r: CGRect);
    fn CGBitmapContextCreateImage(ctx: CFTypeRef) -> CFTypeRef;
    fn CGImageCreateWithImageInRect(img: CFTypeRef, r: CGRect) -> CFTypeRef;
}

const kCFNumberSInt64Type: CFIndex = 4;
const kCFNumberFloat64Type: CFIndex = 6;
const kCFStringEncodingUTF8: u32 = 0x0800_0100;
const kCGColorSpaceModelRGB: i32 = 1;
const kCGInterpolationHigh: i32 = 3;
const kCGImageAlphaPremultipliedLast: u32 = 1;
const kCGImageAlphaNoneSkipLast: u32 = 5;
const kCGImageAlphaNone: u32 = 0;
const kCGImageAlphaNoneSkipFirst: u32 = 6;

/// An owned CoreFoundation object (released on drop).
struct CF(CFTypeRef);

impl CF {
    fn new(r: CFTypeRef) -> Option<Self> {
        (!r.is_null()).then_some(Self(r))
    }
    fn get(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for CF {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

fn cfstr(s: &str) -> CF {
    CF::new(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            s.as_ptr(),
            s.len() as CFIndex,
            kCFStringEncodingUTF8,
            0,
        )
    })
    .expect("CFString")
}

fn cf_string(r: CFTypeRef) -> Option<String> {
    if r.is_null() || unsafe { CFGetTypeID(r) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = vec![0 as c_char; 1024];
    let ok = unsafe {
        CFStringGetCString(
            r,
            buf.as_mut_ptr(),
            buf.len() as CFIndex,
            kCFStringEncodingUTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let c = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    Some(c.to_string_lossy().into_owned())
}

fn cfnum_i64(v: i64) -> CF {
    CF::new(unsafe {
        CFNumberCreate(
            std::ptr::null(),
            kCFNumberSInt64Type,
            (&v as *const i64).cast(),
        )
    })
    .expect("CFNumber")
}

fn cfnum_f64(v: f64) -> CF {
    CF::new(unsafe {
        CFNumberCreate(
            std::ptr::null(),
            kCFNumberFloat64Type,
            (&v as *const f64).cast(),
        )
    })
    .expect("CFNumber")
}

fn num_f64(r: CFTypeRef) -> Option<f64> {
    if r.is_null() || unsafe { CFGetTypeID(r) != CFNumberGetTypeID() } {
        return None;
    }
    let mut v = 0f64;
    unsafe { CFNumberGetValue(r, kCFNumberFloat64Type, (&mut v as *mut f64).cast()) };
    Some(v)
}

fn is_dict(r: CFTypeRef) -> bool {
    !r.is_null() && unsafe { CFGetTypeID(r) == CFDictionaryGetTypeID() }
}

fn dict_get(d: CFTypeRef, k: CFTypeRef) -> CFTypeRef {
    if d.is_null() {
        return std::ptr::null();
    }
    unsafe { CFDictionaryGetValue(d, k) }
}

fn new_dict() -> CF {
    CF::new(unsafe {
        CFDictionaryCreateMutable(
            std::ptr::null(),
            0,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
    })
    .expect("CFDictionary")
}

fn copy_dict(d: CFTypeRef) -> CF {
    CF::new(unsafe { CFDictionaryCreateMutableCopy(std::ptr::null(), 0, d) }).expect("CFDictionary")
}

fn set(d: &CF, k: CFTypeRef, v: CFTypeRef) {
    unsafe { CFDictionarySetValue(d.get(), k, v) }
}

fn file_url(p: &Path) -> anyhow::Result<CF> {
    use std::os::unix::ffi::OsStrExt;
    let b = p.as_os_str().as_bytes();
    CF::new(unsafe {
        CFURLCreateFromFileSystemRepresentation(std::ptr::null(), b.as_ptr(), b.len() as CFIndex, 0)
    })
    .context("bad path")
}

fn open_source(p: &Path) -> anyhow::Result<CF> {
    let url = file_url(p)?;
    let src = CF::new(unsafe { CGImageSourceCreateWithURL(url.get(), std::ptr::null()) })
        .with_context(|| format!("can't open {}", p.display()))?;
    if unsafe { CGImageSourceGetCount(src.get()) } == 0 {
        bail!("not an image (or an unsupported format)");
    }
    Ok(src)
}

/// The UTIs ImageIO can write.
pub fn encodable_utis() -> Vec<String> {
    let Some(a) = CF::new(unsafe { CGImageDestinationCopyTypeIdentifiers() }) else {
        return Vec::new();
    };
    let n = unsafe { CFArrayGetCount(a.get()) };
    (0..n)
        .filter_map(|i| cf_string(unsafe { CFArrayGetValueAtIndex(a.get(), i) }))
        .collect()
}

struct SourceProps {
    props: Option<CF>,
    raw_w: u32,
    raw_h: u32,
    orientation: u32,
}

fn source_props(src: &CF) -> anyhow::Result<SourceProps> {
    let props =
        CF::new(unsafe { CGImageSourceCopyPropertiesAtIndex(src.get(), 0, std::ptr::null()) });
    let p = props.as_ref().map_or(std::ptr::null(), CF::get);
    let w = num_f64(dict_get(p, unsafe { kCGImagePropertyPixelWidth })).unwrap_or(0.0) as u32;
    let h = num_f64(dict_get(p, unsafe { kCGImagePropertyPixelHeight })).unwrap_or(0.0) as u32;
    anyhow::ensure!(w > 0 && h > 0, "can't read the image's size");
    let o = num_f64(dict_get(p, unsafe { kCGImagePropertyOrientation })).unwrap_or(1.0) as u32;
    Ok(SourceProps {
        props,
        raw_w: w,
        raw_h: h,
        orientation: if (1..=8).contains(&o) { o } else { 1 },
    })
}

pub fn image_info(path: &Path) -> anyhow::Result<ImageInfo> {
    let src = open_source(path)?;
    let sp = source_props(&src)?;
    let p = sp.props.as_ref().map_or(std::ptr::null(), CF::get);
    let dpi = |k| num_f64(dict_get(p, k)).filter(|v| *v > 0.0).unwrap_or(72.0);
    let (dx, dy) = unsafe {
        (
            dpi(kCGImagePropertyDPIWidth),
            dpi(kCGImagePropertyDPIHeight),
        )
    };
    let rotated = sp.orientation >= 5;
    let (w, h) = if rotated {
        (sp.raw_h, sp.raw_w)
    } else {
        (sp.raw_w, sp.raw_h)
    };
    let uti = cf_string(unsafe { CGImageSourceGetType(src.get()) });
    Ok(ImageInfo {
        width: w,
        height: h,
        dpi_x: if rotated { dy } else { dx },
        dpi_y: if rotated { dx } else { dy },
        format: uti.as_deref().and_then(OutputFormat::from_uti),
        orientation: sp.orientation,
        has_gps: is_dict(dict_get(p, unsafe { kCGImagePropertyGPSDictionary })),
        has_exif: is_dict(dict_get(p, unsafe { kCGImagePropertyExifDictionary })),
    })
}

/// A decoded, upright image at least `need` (w, h) pixels (or the full
/// size), via the thumbnail API.
fn decode(src: &CF, full: (u32, u32), need: (u32, u32)) -> anyhow::Result<CF> {
    let s = (f64::from(need.0) / f64::from(full.0)).max(f64::from(need.1) / f64::from(full.1));
    let long = full.0.max(full.1);
    let max_px = if s >= 1.0 {
        long
    } else {
        ((f64::from(long) * s).ceil() as u32).clamp(1, long)
    };
    let opts = new_dict();
    unsafe {
        set(
            &opts,
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCFBooleanTrue,
        );
        set(
            &opts,
            kCGImageSourceCreateThumbnailWithTransform,
            kCFBooleanTrue,
        );
        set(&opts, kCGImageSourceShouldCacheImmediately, kCFBooleanTrue);
        let n = cfnum_i64(i64::from(max_px));
        set(&opts, kCGImageSourceThumbnailMaxPixelSize, n.get());
    }
    CF::new(unsafe { CGImageSourceCreateThumbnailAtIndex(src.get(), 0, opts.get()) })
        .context("couldn't decode the image")
}

/// Draw `img` scaled to `w` × `h` (high-quality interpolation). With
/// `opaque` the result has no alpha (white background).
fn scale(img: &CF, w: u32, h: u32, opaque: bool) -> anyhow::Result<CF> {
    let cs_img = unsafe { CGImageGetColorSpace(img.get()) };
    let own_cs;
    let cs =
        if !cs_img.is_null() && unsafe { CGColorSpaceGetModel(cs_img) } == kCGColorSpaceModelRGB {
            cs_img
        } else {
            own_cs = CF::new(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceSRGB) })
                .context("sRGB")?;
            own_cs.get()
        };
    let info = if opaque {
        kCGImageAlphaNoneSkipLast
    } else {
        kCGImageAlphaPremultipliedLast
    };
    let ctx = CF::new(unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w as usize, h as usize, 8, 0, cs, info)
    })
    .context("couldn't create a drawing context (image too large?)")?;
    let r = rect(0.0, 0.0, f64::from(w), f64::from(h));
    unsafe {
        if opaque {
            CGContextSetRGBFillColor(ctx.get(), 1.0, 1.0, 1.0, 1.0);
            CGContextFillRect(ctx.get(), r);
        }
        CGContextSetInterpolationQuality(ctx.get(), kCGInterpolationHigh);
        CGContextDrawImage(ctx.get(), r, img.get());
    }
    CF::new(unsafe { CGBitmapContextCreateImage(ctx.get()) }).context("couldn't scale the image")
}

fn has_alpha(img: &CF) -> bool {
    let a = unsafe { CGImageGetAlphaInfo(img.get()) };
    !matches!(
        a,
        kCGImageAlphaNone | kCGImageAlphaNoneSkipLast | kCGImageAlphaNoneSkipFirst
    )
}

/// The destination properties: quality, upright orientation and (if kept)
/// the original's metadata.
fn dest_props(job: &EncodeJob<'_>, sp: &SourceProps, out: (u32, u32)) -> CF {
    let opts = job.opts;
    let src = sp.props.as_ref().map(CF::get).filter(|p| !p.is_null());
    let d = match src {
        Some(p) if opts.keep_metadata => copy_dict(p),
        _ => new_dict(),
    };
    unsafe {
        // Pixel layout keys are computed by ImageIO.
        for k in [kCGImagePropertyPixelWidth, kCGImagePropertyPixelHeight] {
            CFDictionaryRemoveValue(d.get(), k);
        }
        if let Some(p) = src {
            for k in [kCGImagePropertyDPIWidth, kCGImagePropertyDPIHeight] {
                let v = dict_get(p, k);
                if !v.is_null() {
                    set(&d, k, v);
                }
            }
        }
        let one = cfnum_i64(1);
        set(&d, kCGImagePropertyOrientation, one.get());
        let tiff = dict_get(d.get(), kCGImagePropertyTIFFDictionary);
        if is_dict(tiff) {
            let t = copy_dict(tiff);
            set(&t, kCGImagePropertyTIFFOrientation, one.get());
            set(&d, kCGImagePropertyTIFFDictionary, t.get());
        }
        let exif = dict_get(d.get(), kCGImagePropertyExifDictionary);
        if is_dict(exif) {
            let e = copy_dict(exif);
            let (w, h) = (cfnum_i64(i64::from(out.0)), cfnum_i64(i64::from(out.1)));
            set(&e, kCGImagePropertyExifPixelXDimension, w.get());
            set(&e, kCGImagePropertyExifPixelYDimension, h.get());
            set(&d, kCGImagePropertyExifDictionary, e.get());
        }
        if opts.remove_gps || !opts.keep_metadata {
            CFDictionaryRemoveValue(d.get(), kCGImagePropertyGPSDictionary);
        }
        if job.format.is_lossy() {
            let q = cfnum_f64(f64::from(opts.jpeg_quality.clamp(1, 100)) / 100.0);
            set(&d, kCGImageDestinationLossyCompressionQuality, q.get());
        }
    }
    d
}

pub fn encode(job: &EncodeJob<'_>, info: &ImageInfo, dims: &Dimensions) -> anyhow::Result<()> {
    let src = open_source(job.input)?;
    let sp = source_props(&src)?;
    let full = (info.width, info.height);
    let need = (dims.scaled_width, dims.scaled_height);
    let mut img = decode(&src, full, need)?;
    let opaque = job.format == OutputFormat::Jpeg;
    let (iw, ih) = unsafe {
        (
            CGImageGetWidth(img.get()) as u32,
            CGImageGetHeight(img.get()) as u32,
        )
    };
    if (iw, ih) != need || (opaque && has_alpha(&img)) {
        img = scale(&img, need.0, need.1, opaque)?;
    }
    if let Some(c) = dims.crop {
        img = CF::new(unsafe {
            CGImageCreateWithImageInRect(
                img.get(),
                rect(
                    f64::from(c.x),
                    f64::from(c.y),
                    f64::from(c.width),
                    f64::from(c.height),
                ),
            )
        })
        .context("couldn't crop the image")?;
    }
    let out = dims.output();
    let uti = job.format.uti().context("no output format")?;
    let ty = cfstr(uti);
    let url = file_url(job.output)?;
    let dest = CF::new(unsafe {
        CGImageDestinationCreateWithURL(url.get(), ty.get(), 1, std::ptr::null())
    })
    .with_context(|| format!("can't write {} files here", job.format.label()))?;
    let props = dest_props(job, &sp, out);
    unsafe { CGImageDestinationAddImage(dest.get(), img.get(), props.get()) };
    anyhow::ensure!(
        unsafe { CGImageDestinationFinalize(dest.get()) },
        "couldn't write the {} file",
        job.format.label()
    );
    Ok(())
}

pub fn trash(path: &Path) -> anyhow::Result<()> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))
}

/// Write a `w` × `h` gradient test image (alpha when `alpha`), with an
/// EXIF orientation, an EXIF date and optionally a GPS position.
#[cfg(test)]
pub(crate) fn write_test_image(
    path: &Path,
    w: u32,
    h: u32,
    format: OutputFormat,
    orientation: u32,
    gps: bool,
    dpi: f64,
) -> anyhow::Result<()> {
    let cs = CF::new(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceSRGB) }).context("sRGB")?;
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            px[i] = (x * 255 / w.max(1)) as u8;
            px[i + 1] = (y * 255 / h.max(1)) as u8;
            px[i + 2] = if (x / 8 + y / 8) % 2 == 0 { 200 } else { 40 };
            px[i + 3] = 255;
        }
    }
    let ctx = CF::new(unsafe {
        CGBitmapContextCreate(
            px.as_mut_ptr().cast(),
            w as usize,
            h as usize,
            8,
            (w * 4) as usize,
            cs.get(),
            kCGImageAlphaPremultipliedLast,
        )
    })
    .context("ctx")?;
    let img = CF::new(unsafe { CGBitmapContextCreateImage(ctx.get()) }).context("img")?;
    let ty = cfstr(format.uti().context("format")?);
    let url = file_url(path)?;
    let dest = CF::new(unsafe {
        CGImageDestinationCreateWithURL(url.get(), ty.get(), 1, std::ptr::null())
    })
    .context("dest")?;
    let props = new_dict();
    unsafe {
        let o = cfnum_i64(i64::from(orientation));
        set(&props, kCGImagePropertyOrientation, o.get());
        let d = cfnum_f64(dpi);
        set(&props, kCGImagePropertyDPIWidth, d.get());
        set(&props, kCGImagePropertyDPIHeight, d.get());
        let exif = new_dict();
        let date = cfstr("2024:07:01 12:34:56");
        set(&exif, kCGImagePropertyExifDateTimeOriginal, date.get());
        set(&props, kCGImagePropertyExifDictionary, exif.get());
        if gps {
            let g = new_dict();
            let lat = cfnum_f64(47.5);
            let r = cfstr("N");
            set(&g, kCGImagePropertyGPSLatitude, lat.get());
            set(&g, kCGImagePropertyGPSLatitudeRef, r.get());
            set(&props, kCGImagePropertyGPSDictionary, g.get());
        }
        CGImageDestinationAddImage(dest.get(), img.get(), props.get());
        anyhow::ensure!(CGImageDestinationFinalize(dest.get()), "finalize");
    }
    Ok(())
}

/// The EXIF DateTimeOriginal of an image (tests: metadata kept or not).
#[cfg(test)]
pub(crate) fn exif_date(path: &Path) -> Option<String> {
    let src = open_source(path).ok()?;
    let sp = source_props(&src).ok()?;
    let p = sp.props.as_ref()?.get();
    let e = dict_get(p, unsafe { kCGImagePropertyExifDictionary });
    if !is_dict(e) {
        return None;
    }
    cf_string(dict_get(e, unsafe { kCGImagePropertyExifDateTimeOriginal }))
}

/// The (r, g, b) at the center of an image, decoded at full size (tests).
#[cfg(test)]
pub(crate) fn center_pixel(path: &Path) -> Option<[u8; 3]> {
    let src = open_source(path).ok()?;
    let info = image_info(path).ok()?;
    let img = decode(&src, (info.width, info.height), (info.width, info.height)).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let cs = CF::new(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceSRGB) })?;
    let mut px = vec![0u8; w * h * 4];
    let ctx = CF::new(unsafe {
        CGBitmapContextCreate(
            px.as_mut_ptr().cast(),
            w,
            h,
            8,
            w * 4,
            cs.get(),
            kCGImageAlphaPremultipliedLast,
        )
    })?;
    unsafe { CGContextDrawImage(ctx.get(), rect(0.0, 0.0, w as f64, h as f64), img.get()) };
    drop(ctx);
    let i = ((h / 2) * w + w / 2) * 4;
    Some([px[i], px[i + 1], px[i + 2]])
}
