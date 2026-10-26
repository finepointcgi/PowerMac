//! WIC backend of the Image Resizer (Windows). Compile-checked; not run on
//! Windows yet.
//!
//! Decoder → `IWICBitmapScaler` (high-quality cubic) → `IWICBitmapClipper`
//! (fill crop) → format converter → encoder. JPEG gets `ImageQuality`;
//! metadata is copied block by block when the container format stays the
//! same (`IWICMetadataBlockWriter::InitializeFromBlockReader`), and the GPS
//! block can be removed. EXIF orientation is kept as a tag, as PowerToys
//! does.

use std::path::Path;

use anyhow::Context as _;
use windows::core::{Interface, GUID, HSTRING, PWSTR};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_R4};

use super::{Dimensions, EncodeJob, ImageInfo, OutputFormat};

fn factory() -> anyhow::Result<IWICImagingFactory> {
    unsafe {
        // Fails harmlessly when COM is already initialized on this thread.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
            .context("WIC isn't available")
    }
}

fn container(format: OutputFormat) -> Option<GUID> {
    Some(match format {
        OutputFormat::Jpeg => GUID_ContainerFormatJpeg,
        OutputFormat::Png => GUID_ContainerFormatPng,
        OutputFormat::Tiff => GUID_ContainerFormatTiff,
        OutputFormat::Heic => GUID_ContainerFormatHeif,
        OutputFormat::Webp => GUID_ContainerFormatWebp,
        OutputFormat::Keep => return None,
    })
}

fn format_of(guid: GUID) -> Option<OutputFormat> {
    [
        OutputFormat::Jpeg,
        OutputFormat::Png,
        OutputFormat::Tiff,
        OutputFormat::Heic,
        OutputFormat::Webp,
    ]
    .into_iter()
    .find(|f| container(*f) == Some(guid))
}

fn open(f: &IWICImagingFactory, path: &Path) -> anyhow::Result<IWICBitmapDecoder> {
    let p = HSTRING::from(path.as_os_str());
    unsafe {
        f.CreateDecoderFromFilename(&p, None, GENERIC_READ, WICDecodeMetadataCacheOnLoad)
            .with_context(|| format!("can't open {}", path.display()))
    }
}

pub fn image_info(path: &Path) -> anyhow::Result<ImageInfo> {
    let f = factory()?;
    let dec = open(&f, path)?;
    unsafe {
        let frame = dec.GetFrame(0)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h)?;
        let (mut dx, mut dy) = (0f64, 0f64);
        let _ = frame.GetResolution(&mut dx, &mut dy);
        let has_gps = frame.GetMetadataQueryReader().ok().is_some_and(|r| {
            let mut v = Default::default();
            r.GetMetadataByName(&HSTRING::from("/app1/ifd/gps"), &mut v)
                .is_ok()
        });
        Ok(ImageInfo {
            width: w,
            height: h,
            dpi_x: if dx > 0.0 { dx } else { 96.0 },
            dpi_y: if dy > 0.0 { dy } else { 96.0 },
            format: dec.GetContainerFormat().ok().and_then(format_of),
            orientation: 1,
            has_gps,
            has_exif: false,
        })
    }
}

fn quality_variant(q: f32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_R4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { fltVal: q },
            }),
        },
    }
}

pub fn encode(job: &EncodeJob<'_>, info: &ImageInfo, dims: &Dimensions) -> anyhow::Result<()> {
    let f = factory()?;
    let dec = open(&f, job.input)?;
    let out_guid = container(job.format).context("no output format")?;
    unsafe {
        let frame = dec.GetFrame(0)?;
        let scaler = f.CreateBitmapScaler()?;
        scaler.Initialize(
            &frame,
            dims.scaled_width,
            dims.scaled_height,
            WICBitmapInterpolationModeHighQualityCubic,
        )?;
        let mut source: IWICBitmapSource = scaler.cast()?;
        if let Some(c) = dims.crop {
            let clipper = f.CreateBitmapClipper()?;
            let r = WICRect {
                X: c.x as i32,
                Y: c.y as i32,
                Width: c.width as i32,
                Height: c.height as i32,
            };
            clipper.Initialize(&source, &r)?;
            source = clipper.cast()?;
        }
        let mut pf = if job.format == OutputFormat::Jpeg {
            GUID_WICPixelFormat24bppBGR
        } else {
            GUID_WICPixelFormat32bppBGRA
        };
        let conv = f.CreateFormatConverter()?;
        conv.Initialize(
            &source,
            &pf,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;

        let stream = f.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(job.output.as_os_str()), GENERIC_WRITE.0)?;
        let enc = f.CreateEncoder(&out_guid, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut fe: Option<IWICBitmapFrameEncode> = None;
        let mut bag: Option<IPropertyBag2> = None;
        enc.CreateNewFrame(&mut fe, &mut bag)?;
        let fe = fe.context("no frame encoder")?;
        if let Some(bag) = &bag {
            if job.format == OutputFormat::Jpeg {
                let mut name: Vec<u16> = "ImageQuality\0".encode_utf16().collect();
                let prop = PROPBAG2 {
                    pstrName: PWSTR(name.as_mut_ptr()),
                    ..Default::default()
                };
                let v = quality_variant(f32::from(job.opts.jpeg_quality.clamp(1, 100)) / 100.0);
                let _ = bag.Write(1, &prop, &v);
            }
        }
        fe.Initialize(bag.as_ref())?;
        let (w, h) = dims.output();
        fe.SetSize(w, h)?;
        fe.SetResolution(info.dpi_x, info.dpi_y)?;
        fe.SetPixelFormat(&mut pf)?;

        // Metadata: copy the blocks when the container stays the same.
        let same = dec.GetContainerFormat().ok() == Some(out_guid);
        if job.opts.keep_metadata && same {
            if let (Ok(reader), Ok(writer)) = (
                frame.cast::<IWICMetadataBlockReader>(),
                fe.cast::<IWICMetadataBlockWriter>(),
            ) {
                let _ = writer.InitializeFromBlockReader(&reader);
                if job.opts.remove_gps {
                    if let Ok(qw) = fe.GetMetadataQueryWriter() {
                        let _ = qw.RemoveMetadataByName(&HSTRING::from("/app1/ifd/gps"));
                    }
                }
            }
        }

        fe.WriteSource(&conv, std::ptr::null())?;
        fe.Commit()?;
        enc.Commit()?;
    }
    Ok(())
}
