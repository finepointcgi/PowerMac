//! The Mirror widget's camera preview (AVFoundation, loaded at runtime).
//! Created on the main thread; the session starts/stops on a background
//! thread (startRunning blocks). Stopped whenever the panel collapses.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, Bool};
use objc2::msg_send;
use objc2_foundation::NSString;

use super::ffi::ns;
use super::rt::{self, SendObj};
use crate::widgets::Access;

const VIDEO: &str = "vide"; // AVMediaTypeVideo

fn av_class(name: &str) -> Option<&'static objc2::runtime::AnyClass> {
    if rt::class_named("AVCaptureDevice").is_none() {
        rt::load_framework("/System/Library/Frameworks/AVFoundation.framework");
    }
    rt::class_named(name)
}

/// Authorization, read-only.
pub fn access() -> Access {
    let Some(dev) = av_class("AVCaptureDevice") else { return Access::Unavailable };
    let st: isize = unsafe { msg_send![dev, authorizationStatusForMediaType: &*ns(VIDEO)] };
    match st {
        // Without the usage key, asking would terminate the app.
        0 if !rt::info_plist_has("NSCameraUsageDescription") => Access::MissingUsageKey,
        0 => Access::NotDetermined,
        3 => Access::Granted,
        _ => Access::Denied,
    }
}

pub struct Camera {
    session: Option<Retained<AnyObject>>,
    pub preview: Option<Retained<AnyObject>>,
    /// Permission callback: 0 pending, 1 granted, 2 denied.
    answer: Arc<AtomicU8>,
    pub want: bool,
}

impl Camera {
    pub fn new() -> Self {
        Camera { session: None, preview: None, answer: Arc::new(AtomicU8::new(0)), want: false }
    }

    pub fn running(&self) -> bool {
        self.session.is_some()
    }

    /// Start (asking for permission first if needed). Returns the access
    /// state to show.
    pub fn start(&mut self) -> Access {
        self.want = true;
        match access() {
            Access::Granted => {
                self.start_session();
                Access::Granted
            }
            Access::NotDetermined => {
                let Some(dev) = av_class("AVCaptureDevice") else { return Access::Unavailable };
                let answer = self.answer.clone();
                let block = RcBlock::new(move |ok: Bool| answer.store(if ok.as_bool() { 1 } else { 2 }, Ordering::SeqCst));
                unsafe {
                    let _: () = msg_send![dev, requestAccessForMediaType: &*ns(VIDEO), completionHandler: &*block];
                }
                Access::Requesting
            }
            other => {
                self.want = false;
                other
            }
        }
    }

    /// Call each frame while waiting for permission.
    pub fn poll(&mut self) -> Option<Access> {
        match self.answer.swap(0, Ordering::SeqCst) {
            1 => {
                if self.want {
                    self.start_session();
                }
                Some(Access::Granted)
            }
            2 => {
                self.want = false;
                Some(Access::Denied)
            }
            _ => None,
        }
    }

    fn start_session(&mut self) {
        if self.session.is_some() {
            return;
        }
        autoreleasepool(|_| unsafe {
            let (Some(dev_c), Some(sess_c), Some(input_c), Some(prev_c)) =
                (av_class("AVCaptureDevice"), av_class("AVCaptureSession"), av_class("AVCaptureDeviceInput"), av_class("AVCaptureVideoPreviewLayer"))
            else {
                return;
            };
            let dev: Option<Retained<AnyObject>> = msg_send![dev_c, defaultDeviceWithMediaType: &*ns(VIDEO)];
            let Some(dev) = dev else { return };
            let input: Option<Retained<AnyObject>> = msg_send![input_c, deviceInputWithDevice: &*dev, error: std::ptr::null_mut::<*mut AnyObject>()];
            let Some(input) = input else { return };
            let a: objc2::rc::Allocated<AnyObject> = msg_send![sess_c, alloc];
            let session: Retained<AnyObject> = msg_send![a, init];
            let ok: bool = msg_send![&*session, canAddInput: &*input];
            if !ok {
                return;
            }
            let _: () = msg_send![&*session, addInput: &*input];
            let preview: Retained<AnyObject> = msg_send![prev_c, layerWithSession: &*session];
            let _: () = msg_send![&*preview, setVideoGravity: &*NSString::from_str("AVLayerVideoGravityResizeAspectFill")];
            let conn: Option<Retained<AnyObject>> = msg_send![&*preview, connection];
            if let Some(c) = conn {
                let sup: bool = msg_send![&*c, isVideoMirroringSupported];
                if sup {
                    let _: () = msg_send![&*c, setAutomaticallyAdjustsVideoMirroring: false];
                    let _: () = msg_send![&*c, setVideoMirrored: true];
                }
            }
            let s = SendObj(session.clone());
            std::thread::spawn(move || {
                let s = s;
                autoreleasepool(|_| {
                    let _: () = msg_send![&*s.0, startRunning];
                });
            });
            self.session = Some(session);
            self.preview = Some(preview);
        });
        log::info!("notch-shelf: camera preview started");
    }

    /// Stop the camera (the indicator light goes off).
    pub fn stop(&mut self) {
        self.want = false;
        if let Some(p) = self.preview.take() {
            unsafe {
                let _: () = msg_send![&*p, removeFromSuperlayer];
            }
        }
        if let Some(s) = self.session.take() {
            let s = SendObj(s);
            std::thread::spawn(move || {
                let s = s;
                autoreleasepool(|_| unsafe {
                    let _: () = msg_send![&*s.0, stopRunning];
                });
            });
            log::info!("notch-shelf: camera preview stopped");
        }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.stop();
    }
}
