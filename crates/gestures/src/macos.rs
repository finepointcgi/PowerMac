//! macOS: touch frames from the private MultitouchSupport framework
//! (loaded at runtime with `dlopen`, so a missing / changed framework only
//! makes gestures unavailable), and physical clicks from the shared input
//! hook.

use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, OnceLock, RwLock};

use crate::engine::{Msg, Shared};
use crate::{Contact, DeviceInfo, Frame};

const FRAMEWORK: &CStr =
    c"/System/Library/PrivateFrameworks/MultitouchSupport.framework/MultitouchSupport";

type MTDeviceRef = *mut c_void;
type CFArrayRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MTPoint {
    x: f32,
    y: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MTVector {
    pos: MTPoint,
    vel: MTPoint,
}

/// `MTTouch` as reverse-engineered (OpenMultitouchSupport and others);
/// stable since 10.5.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MTTouch {
    frame: i32,
    timestamp: f64,
    path_index: i32,
    state: i32,
    finger_id: i32,
    hand_id: i32,
    normalized: MTVector,
    z_total: f32,
    field9: i32,
    angle: f32,
    major_axis: f32,
    minor_axis: f32,
    absolute: MTVector,
    field14: i32,
    field15: i32,
    z_density: f32,
}

const _: () = assert!(std::mem::size_of::<MTTouch>() == 96);

/// `MTTouchStateMakeTouch` / `MTTouchStateTouching`.
const STATE_MAKE_TOUCH: i32 = 3;
const STATE_TOUCHING: i32 = 4;

/// `(device, touches, count, timestamp, frame)`. The count and frame are
/// `int` or `size_t` depending on the header; only the low 32 bits are read.
type FrameCallback = unsafe extern "C" fn(MTDeviceRef, *const MTTouch, i32, f64, i32) -> i32;

type CreateListFn = unsafe extern "C" fn() -> CFArrayRef;
type RegisterFn = unsafe extern "C" fn(MTDeviceRef, FrameCallback);
type StartFn = unsafe extern "C" fn(MTDeviceRef, i32) -> i32;
type StopFn = unsafe extern "C" fn(MTDeviceRef) -> i32;
type GetIdFn = unsafe extern "C" fn(MTDeviceRef, *mut u64) -> i32;
type IsBuiltInFn = unsafe extern "C" fn(MTDeviceRef) -> bool;
type GetFamilyFn = unsafe extern "C" fn(MTDeviceRef, *mut i32) -> i32;

#[allow(non_snake_case)]
struct Api {
    MTDeviceCreateList: CreateListFn,
    MTRegisterContactFrameCallback: RegisterFn,
    MTUnregisterContactFrameCallback: Option<RegisterFn>,
    MTDeviceStart: StartFn,
    MTDeviceStop: StopFn,
    MTDeviceGetDeviceID: Option<GetIdFn>,
    MTDeviceIsBuiltIn: Option<IsBuiltInFn>,
    MTDeviceGetFamilyID: Option<GetFamilyFn>,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(a: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFArrayRef, i: isize) -> *const c_void;
    fn CFRelease(p: *const c_void);
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(|e| e.clone())
}

fn load() -> Result<Api, String> {
    // SAFETY: dlopen/dlsym with valid C strings; the library stays loaded
    // for the life of the process (never dlclose'd), and each symbol is cast
    // to the signature it is known to have.
    unsafe {
        let h = libc::dlopen(FRAMEWORK.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        if h.is_null() {
            let err = libc::dlerror();
            let msg = if err.is_null() {
                "unknown error".into()
            } else {
                CStr::from_ptr(err).to_string_lossy().into_owned()
            };
            return Err(format!(
                "gestures unavailable: can't load MultitouchSupport ({msg})"
            ));
        }
        let sym = |name: &CStr| {
            let p = libc::dlsym(h, name.as_ptr());
            (!p.is_null()).then_some(p)
        };
        macro_rules! req {
            ($n:literal, $t:ty) => {
                match sym($n) {
                    Some(p) => std::mem::transmute::<*mut c_void, $t>(p),
                    None => {
                        return Err(format!(
                            "gestures unavailable: MultitouchSupport has no {}",
                            $n.to_string_lossy()
                        ))
                    }
                }
            };
        }
        macro_rules! opt {
            ($n:literal, $t:ty) => {
                sym($n).map(|p| std::mem::transmute::<*mut c_void, $t>(p))
            };
        }
        Ok(Api {
            MTDeviceCreateList: req!(c"MTDeviceCreateList", CreateListFn),
            MTRegisterContactFrameCallback: req!(c"MTRegisterContactFrameCallback", RegisterFn),
            MTUnregisterContactFrameCallback: opt!(c"MTUnregisterContactFrameCallback", RegisterFn),
            MTDeviceStart: req!(c"MTDeviceStart", StartFn),
            MTDeviceStop: req!(c"MTDeviceStop", StopFn),
            MTDeviceGetDeviceID: opt!(c"MTDeviceGetDeviceID", GetIdFn),
            MTDeviceIsBuiltIn: opt!(c"MTDeviceIsBuiltIn", IsBuiltInFn),
            MTDeviceGetFamilyID: opt!(c"MTDeviceGetFamilyID", GetFamilyFn),
        })
    }
}

/// # Safety
/// `d` must be a live device from `MTDeviceCreateList`.
unsafe fn device_info(api: &Api, d: MTDeviceRef) -> DeviceInfo {
    let mut id = 0u64;
    if let Some(f) = api.MTDeviceGetDeviceID {
        f(d, &mut id);
    }
    let mut family = -1i32;
    if let Some(f) = api.MTDeviceGetFamilyID {
        f(d, &mut family);
    }
    DeviceInfo {
        id,
        built_in: api.MTDeviceIsBuiltIn.is_some_and(|f| f(d)),
        family_id: family,
    }
}

/// The devices MultitouchSupport reports, without starting them.
pub(crate) fn availability() -> Result<Vec<DeviceInfo>, String> {
    let api = api()?;
    // SAFETY: the list is +1 and released after reading it.
    unsafe {
        let list = (api.MTDeviceCreateList)();
        if list.is_null() {
            return Err("gestures unavailable: no multitouch device list".into());
        }
        let n = CFArrayGetCount(list);
        let v = (0..n)
            .map(|i| device_info(api, CFArrayGetValueAtIndex(list, i) as MTDeviceRef))
            .collect();
        CFRelease(list);
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

pub mod reader {
    //! Raw touch frames from every multitouch device (read-only: macOS
    //! still sees and handles every touch). One reader per process.

    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Frame consumer: `(device key, frame)`. Runs on MultitouchSupport's
    /// thread at the trackpad's frame rate; keep it short.
    pub type OnFrame = Box<dyn Fn(usize, &Frame) + Send + Sync>;

    static SINK: RwLock<Option<OnFrame>> = RwLock::new(None);
    static FRAMES: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn on_frame(
        dev: MTDeviceRef,
        touches: *const MTTouch,
        n: i32,
        ts: f64,
        _frame: i32,
    ) -> i32 {
        let res = std::panic::catch_unwind(|| {
            FRAMES.fetch_add(1, Ordering::Relaxed);
            let n = n.clamp(0, 32) as usize;
            let contacts: Vec<Contact> = if touches.is_null() {
                Vec::new()
            } else {
                // SAFETY: MultitouchSupport passes `n` touches.
                std::slice::from_raw_parts(touches, n)
                    .iter()
                    .map(|t| Contact {
                        id: t.path_index,
                        x: t.normalized.pos.x,
                        y: t.normalized.pos.y,
                        touching: t.state == STATE_MAKE_TOUCH || t.state == STATE_TOUCHING,
                    })
                    .collect()
            };
            if let Ok(g) = SINK.read() {
                if let Some(f) = g.as_ref() {
                    f(dev as usize, &Frame { time: ts, contacts });
                }
            }
        });
        let _ = res;
        0
    }

    struct Dev {
        ptr: usize,
        info: DeviceInfo,
    }

    struct State {
        devices: Vec<Dev>,
        /// The `MTDeviceCreateList` arrays that own the started devices.
        lists: Vec<usize>,
    }

    /// Reads touch frames until dropped.
    pub struct Reader {
        state: Arc<Mutex<State>>,
        stop: Option<mpsc::Sender<()>>,
        rescan: Option<std::thread::JoinHandle<()>>,
    }

    /// How often newly connected trackpads are picked up.
    pub const RESCAN: Duration = Duration::from_secs(3);

    /// Start every device not started yet. Returns how many were added.
    fn scan(api: &Api, st: &mut State) -> usize {
        // SAFETY: devices come from the +1 list, which is kept (owning
        // them) while any of its devices runs, else released.
        unsafe {
            let list = (api.MTDeviceCreateList)();
            if list.is_null() {
                return 0;
            }
            let mut added = 0;
            for i in 0..CFArrayGetCount(list) {
                let d = CFArrayGetValueAtIndex(list, i) as MTDeviceRef;
                if d.is_null() {
                    continue;
                }
                let info = device_info(api, d);
                let known = st
                    .devices
                    .iter()
                    .any(|k| k.ptr == d as usize || (info.id != 0 && k.info.id == info.id));
                if known {
                    continue;
                }
                (api.MTRegisterContactFrameCallback)(d, on_frame);
                (api.MTDeviceStart)(d, 0);
                st.devices.push(Dev {
                    ptr: d as usize,
                    info,
                });
                added += 1;
            }
            if added > 0 {
                st.lists.push(list as usize);
            } else {
                CFRelease(list);
            }
            added
        }
    }

    impl Reader {
        /// Load the framework and start every multitouch device. Fails
        /// ("gestures unavailable: …") when the framework or a needed
        /// symbol is missing, or there is no device yet *and* devices can't
        /// be told apart for hot-plug.
        pub fn start(sink: OnFrame) -> Result<Reader, String> {
            let api = api()?;
            if let Ok(mut g) = SINK.write() {
                *g = Some(sink);
            }
            let state = Arc::new(Mutex::new(State {
                devices: Vec::new(),
                lists: Vec::new(),
            }));
            let n = scan(api, &mut state.lock().unwrap());
            log::info!("gestures: started {n} multitouch device(s)");
            let (tx, rx) = mpsc::channel::<()>();
            let rescan = if api.MTDeviceGetDeviceID.is_some() {
                let st = state.clone();
                std::thread::Builder::new()
                    .name("gestures-rescan".into())
                    .spawn(move || {
                        while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(RESCAN) {
                            if let Ok(mut s) = st.lock() {
                                let added = scan(api, &mut s);
                                if added > 0 {
                                    log::info!(
                                        "gestures: started {added} new multitouch device(s)"
                                    );
                                }
                            }
                        }
                    })
                    .ok()
            } else {
                if n == 0 {
                    if let Ok(mut g) = SINK.write() {
                        *g = None;
                    }
                    return Err("gestures unavailable: no multitouch trackpad found".into());
                }
                None
            };
            Ok(Reader {
                state,
                stop: Some(tx),
                rescan,
            })
        }

        pub fn devices(&self) -> Vec<DeviceInfo> {
            self.state
                .lock()
                .map(|s| s.devices.iter().map(|d| d.info.clone()).collect())
                .unwrap_or_default()
        }

        /// Frames received by the callback since the process started.
        pub fn frames_seen() -> u32 {
            FRAMES.load(Ordering::Relaxed)
        }
    }

    impl Drop for Reader {
        fn drop(&mut self) {
            self.stop.take();
            if let Some(t) = self.rescan.take() {
                let _ = t.join();
            }
            if let Ok(mut g) = SINK.write() {
                *g = None;
            }
            let Ok(api) = api() else { return };
            if let Ok(mut s) = self.state.lock() {
                // SAFETY: the devices are owned by the lists released last.
                unsafe {
                    for d in s.devices.drain(..) {
                        let p = d.ptr as MTDeviceRef;
                        if let Some(un) = api.MTUnregisterContactFrameCallback {
                            un(p, on_frame);
                        }
                        (api.MTDeviceStop)(p);
                    }
                    for l in s.lists.drain(..) {
                        CFRelease(l as CFArrayRef);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Clicks
// ---------------------------------------------------------------------------

/// Watches left clicks in the shared input hook: tells the recognizer about
/// every physical click (a click is not a tap) and swallows the click of a
/// bound N-finger click.
pub(crate) struct ClickHook {
    _guard: pm_hook::Guard,
}

impl ClickHook {
    pub(crate) fn start(shared: Arc<Shared>, tx: mpsc::Sender<Msg>) -> Result<ClickHook, String> {
        use pm_hook::{EventMask, EventType, Subscription, Verdict};
        let mask = EventMask::of(&[
            EventType::LeftMouseDown,
            EventType::LeftMouseUp,
            EventType::LeftMouseDragged,
        ]);
        let mut swallowing = false;
        let guard = pm_hook::subscribe(
            Subscription::new("gestures", crate::PRIORITY, mask, move |ev| {
                match ev.kind() {
                    EventType::LeftMouseDown => {
                        let n = shared.fingers.load(Ordering::Relaxed);
                        if n == 0 {
                            return Verdict::Pass; // a mouse, not the trackpad
                        }
                        let _ = tx.send(Msg::Click);
                        if shared.should_swallow_click(n) {
                            swallowing = true;
                            Verdict::Swallow
                        } else {
                            Verdict::Pass
                        }
                    }
                    EventType::LeftMouseDragged if swallowing => Verdict::Swallow,
                    EventType::LeftMouseUp if swallowing => {
                        swallowing = false;
                        Verdict::Swallow
                    }
                    _ => Verdict::Pass,
                }
            })
            .ignore_tags(&[
                pm_hook::tags::MWB,
                pm_hook::tags::KEYBOARD_MANAGER,
                pm_hook::tags::MOUSE_TUNING,
            ]),
        );
        let st = pm_hook::status();
        if !st.is_active() {
            drop(guard);
            return Err(format!("the input hook isn't active ({:?}); taps can't be told from clicks and N-finger clicks don't fire", st.state));
        }
        Ok(ClickHook { _guard: guard })
    }
}
