//! Minimal CoreGraphics bindings: owned `CGImage`s, building one from pixels,
//! and the Screen Recording permission calls.
#![allow(non_upper_case_globals)]

use std::ffi::c_void;

use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::Retained;
use objc2_foundation::NSData;

pub type CFStringRef = *const c_void;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static kCGColorSpaceSRGB: CFStringRef;
    fn CGColorSpaceCreateWithName(name: CFStringRef) -> *mut c_void;
    fn CGColorSpaceRelease(cs: *mut c_void);
    fn CGDataProviderCreateWithCFData(data: *const c_void) -> *mut c_void;
    fn CGDataProviderRelease(p: *mut c_void);
    #[allow(clippy::too_many_arguments)]
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        bitmap_info: u32,
        provider: *mut c_void,
        decode: *const f64,
        should_interpolate: bool,
        intent: i32,
    ) -> *mut c_void;
    pub fn CGImageRetain(img: *const c_void) -> *const c_void;
    fn CGImageRelease(img: *const c_void);
    #[cfg(test)]
    fn CGImageGetWidth(img: *const c_void) -> usize;
    #[cfg(test)]
    fn CGImageGetHeight(img: *const c_void) -> usize;
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGRequestScreenCaptureAccess() -> bool;
}

/// `CGImageRef` with the right Objective-C encoding for `msg_send!`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct CgImageRef(pub *const c_void);

unsafe impl Encode for CgImageRef {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
}
unsafe impl RefEncode for CgImageRef {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

/// An owned (retained) CGImage.
pub struct CgImage(*const c_void);

// CGImage is immutable and thread-safe.
unsafe impl Send for CgImage {}

impl CgImage {
    /// Take ownership of a +1 reference.
    ///
    /// # Safety
    /// `p` must be a valid CGImageRef the caller owns (or null).
    pub unsafe fn from_owned(p: *const c_void) -> Option<Self> {
        (!p.is_null()).then_some(Self(p))
    }

    pub fn as_ref(&self) -> CgImageRef {
        CgImageRef(self.0)
    }

    #[cfg(test)]
    pub fn size(&self) -> (usize, usize) {
        unsafe { (CGImageGetWidth(self.0), CGImageGetHeight(self.0)) }
    }

    /// An sRGB image from straight RGBA pixels, flattened onto white (text
    /// on a transparent background would otherwise turn invisible).
    pub fn from_rgba(width: usize, height: usize, rgba: &[u8]) -> Option<Self> {
        let mut px = Vec::with_capacity(width * height * 4);
        for p in rgba.as_chunks::<4>().0.iter().take(width * height) {
            let a = p[3] as u32;
            let over = |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
            px.extend_from_slice(&[over(p[0]), over(p[1]), over(p[2]), 255]);
        }
        if px.len() != width * height * 4 {
            return None;
        }
        let data: Retained<NSData> = NSData::with_bytes(&px);
        unsafe {
            let cs = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
            let provider = CGDataProviderCreateWithCFData(Retained::as_ptr(&data) as *const c_void);
            // kCGImageAlphaNoneSkipLast (5), default byte order: RGBX.
            let img = CGImageCreate(
                width,
                height,
                8,
                32,
                width * 4,
                cs,
                5,
                provider,
                std::ptr::null(),
                false,
                0,
            );
            CGDataProviderRelease(provider);
            CGColorSpaceRelease(cs);
            Self::from_owned(img)
        }
    }
}

impl Drop for CgImage {
    fn drop(&mut self) {
        unsafe { CGImageRelease(self.0) }
    }
}
