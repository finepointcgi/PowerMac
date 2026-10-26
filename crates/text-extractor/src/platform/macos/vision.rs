//! OCR with Vision's `VNRecognizeTextRequest` (accurate level), called
//! through the Objective-C runtime.

use std::path::Path;

use anyhow::{anyhow, Result};
use objc2::rc::{autoreleasepool, Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{msg_send, sel};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSRect, NSString, NSURL};

use super::cg::CgImageRef;
use crate::text::Fragment;
use crate::Config;

#[link(name = "Vision", kind = "framework")]
extern "C" {}

pub enum Source<'a> {
    Image(CgImageRef),
    File(&'a Path),
}

fn class(name: &std::ffi::CStr) -> Result<&'static AnyClass> {
    AnyClass::get(name).ok_or_else(|| {
        anyhow!(
            "{} is not available (macOS 10.15 or later is needed)",
            name.to_string_lossy()
        )
    })
}

fn responds(obj: &AnyObject, s: objc2::runtime::Sel) -> bool {
    let r: Bool = unsafe { msg_send![obj, respondsToSelector: s] };
    r.as_bool()
}

fn new_request(cfg: &Config) -> Result<Retained<AnyObject>> {
    let req: Retained<AnyObject> = unsafe { msg_send![class(c"VNRecognizeTextRequest")?, new] };
    unsafe {
        // VNRequestTextRecognitionLevelAccurate
        let _: () = msg_send![&*req, setRecognitionLevel: 0isize];
        let _: () = msg_send![&*req, setUsesLanguageCorrection: cfg.language_correction];
    }
    let langs: Vec<Retained<NSString>> = cfg
        .languages
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(NSString::from_str)
        .collect();
    if langs.is_empty() {
        // macOS 13+: pick the language per image.
        if responds(&req, sel!(setAutomaticallyDetectsLanguage:)) {
            let _: () = unsafe { msg_send![&*req, setAutomaticallyDetectsLanguage: true] };
        }
    } else {
        let arr = NSArray::from_retained_slice(&langs);
        let _: () = unsafe { msg_send![&*req, setRecognitionLanguages: &*arr] };
    }
    Ok(req)
}

/// Recognize the text in `src`; fragments are in normalized top-left coordinates.
pub fn recognize(src: Source<'_>, cfg: &Config) -> Result<Vec<Fragment>> {
    autoreleasepool(|_| {
        let handler_cls = class(c"VNImageRequestHandler")?;
        let opts = NSDictionary::<NSString, AnyObject>::new();
        let alloc: Allocated<AnyObject> = unsafe { msg_send![handler_cls, alloc] };
        let handler: Option<Retained<AnyObject>> = match src {
            Source::Image(img) => unsafe {
                msg_send![alloc, initWithCGImage: img, options: &*opts]
            },
            Source::File(p) => {
                let url = NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()));
                unsafe { msg_send![alloc, initWithURL: &*url, options: &*opts] }
            }
        };
        let handler = handler.ok_or_else(|| anyhow!("could not read the image"))?;
        let req = new_request(cfg)?;
        let reqs = NSArray::from_retained_slice(std::slice::from_ref(&req));
        let res: std::result::Result<(), Retained<NSError>> =
            unsafe { msg_send![&*handler, performRequests: &*reqs, error: _] };
        res.map_err(|e| anyhow!("text recognition failed: {}", e.localizedDescription()))?;
        let results: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![&*req, results] };
        let mut out = Vec::new();
        for obs in results.iter().flat_map(|a| a.iter()) {
            let cands: Retained<NSArray<AnyObject>> =
                unsafe { msg_send![&*obs, topCandidates: 1usize] };
            let Some(best) = cands.firstObject() else {
                continue;
            };
            let s: Retained<NSString> = unsafe { msg_send![&*best, string] };
            // Normalized, origin bottom-left.
            let bb: NSRect = unsafe { msg_send![&*obs, boundingBox] };
            out.push(Fragment::new(
                s.to_string(),
                bb.origin.x,
                1.0 - bb.origin.y - bb.size.height,
                bb.size.width,
                bb.size.height,
            ));
        }
        Ok(out)
    })
}

/// Languages Vision can recognize at the accurate level.
pub fn supported_languages() -> Vec<String> {
    autoreleasepool(|_| {
        let Ok(req) = new_request(&Config::default()) else {
            return Vec::new();
        };
        let res: std::result::Result<Retained<NSArray<NSString>>, Retained<NSError>> =
            unsafe { msg_send![&*req, supportedRecognitionLanguagesAndReturnError: _] };
        res.map(|a| a.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    })
}
