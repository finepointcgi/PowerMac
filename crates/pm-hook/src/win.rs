//! Windows backend: one `WH_KEYBOARD_LL` and one `WH_MOUSE_LL` hook on the
//! `pm-hook` thread, plus [`Forwarder`] for handlers whose state lives on a
//! GUI thread.
//!
//! Each hook is installed only while a subscriber wants that kind of event.
//! Low-level hook callbacks must return within `LowLevelHooksTimeout`
//! (about 1 s); after a slower dispatch the hooks are reinstalled, since
//! Windows 7+ silently removes a hook that timed out. A 500 ms timer retries
//! a hook that could not be installed.

#![allow(clippy::upper_case_acronyms)]

use std::cell::{Cell, RefCell};
use std::mem::{size_of, zeroed};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
    PeekMessageW, PostThreadMessageW, RegisterClassW, SendMessageTimeoutW, SetTimer, SetWindowLongPtrW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, GWLP_USERDATA, HHOOK, HWND_MESSAGE, KBDLLHOOKSTRUCT,
    LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, SMTO_ABORTIFHUNG, SMTO_NORMAL, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WM_TIMER, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW,
};

use crate::hub::{mark_dispatch_thread, Backend, Hub, Out};
use crate::{Event, EventType, Raw, Synthetic, TapState};

const WM_RECONCILE: u32 = WM_APP + 0x51;
const WM_REINSTALL: u32 = WM_APP + 0x52;
const WM_FORWARD: u32 = WM_APP + 0x53;
/// A dispatch slower than this risks the hook being removed.
const HOOK_TIMEOUT_RISK: Duration = Duration::from_millis(900);

#[derive(Default)]
struct Ctl {
    started: bool,
    wanted: u64,
    applied: u64,
    thread_id: u32,
}

struct Shared {
    ctl: Mutex<Ctl>,
    cv: Condvar,
}

fn lock(s: &Shared) -> MutexGuard<'_, Ctl> {
    s.ctl.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) struct WinBackend {
    shared: Arc<Shared>,
}

impl WinBackend {
    pub(crate) fn new() -> Self {
        WinBackend { shared: Arc::new(Shared { ctl: Mutex::new(Ctl::default()), cv: Condvar::new() }) }
    }
}

impl Backend for WinBackend {
    fn reconcile(&self, hub: &Arc<Hub>, wait: bool) {
        let (gen, start, tid) = {
            let mut c = lock(&self.shared);
            c.wanted += 1;
            let start = !c.started;
            c.started = true;
            (c.wanted, start, c.thread_id)
        };
        if start {
            hub.update_status(|s| s.state = TapState::Starting);
            let (h, s) = (hub.clone(), self.shared.clone());
            if let Err(e) = std::thread::Builder::new().name("pm-hook".into()).spawn(move || thread_main(h, s)) {
                lock(&self.shared).started = false;
                hub.update_status(|s| s.state = TapState::Unavailable(format!("could not start the hook thread: {e}")));
                return;
            }
        } else if tid != 0 {
            unsafe { PostThreadMessageW(tid, WM_RECONCILE, 0, 0) };
        }
        if wait {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut c = lock(&self.shared);
            while c.applied < gen {
                let now = Instant::now();
                if now >= deadline {
                    log::warn!("pm-hook: the hook thread did not apply a subscriber change within 5 s");
                    break;
                }
                c = self.shared.cv.wait_timeout(c, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
    }

    fn recurse_synthetics(&self) -> bool {
        false
    }
}

struct HookThread {
    hub: Arc<Hub>,
    kb: Cell<HHOOK>,
    ms: Cell<HHOOK>,
    thread_id: u32,
}

thread_local! {
    static CTX: RefCell<Option<Rc<HookThread>>> = const { RefCell::new(None) };
}

fn ctx() -> Option<Rc<HookThread>> {
    CTX.with(|c| c.borrow().clone())
}

fn thread_main(hub: Arc<Hub>, shared: Arc<Shared>) {
    mark_dispatch_thread(true);
    unsafe {
        let mut msg: MSG = zeroed();
        PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
        let tid = GetCurrentThreadId();
        let ht = Rc::new(HookThread { hub: hub.clone(), kb: Cell::new(std::ptr::null_mut()), ms: Cell::new(std::ptr::null_mut()), thread_id: tid });
        CTX.with(|c| *c.borrow_mut() = Some(ht.clone()));
        lock(&shared).thread_id = tid;
        let timer = SetTimer(std::ptr::null_mut(), 0, 500, None);
        apply(&ht, &shared, false);
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.hwnd.is_null() {
                match msg.message {
                    WM_RECONCILE => {
                        apply(&ht, &shared, false);
                        continue;
                    }
                    WM_REINSTALL => {
                        apply(&ht, &shared, true);
                        continue;
                    }
                    WM_TIMER if msg.wParam == timer => {
                        // Retry a hook that failed to install.
                        let st = hub.status().state;
                        if matches!(st, TapState::Unavailable(_)) {
                            apply(&ht, &shared, false);
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Install / remove the hooks to match the subscribers' masks.
unsafe fn apply(ht: &HookThread, shared: &Shared, reinstall: bool) {
    let gen = lock(shared).wanted;
    let mask = ht.hub.union_mask();
    let hmod = GetModuleHandleW(std::ptr::null());
    let mut failed = Vec::new();
    for (want, cell, id, proc_, what) in [
        (mask.has_keyboard(), &ht.kb, WH_KEYBOARD_LL, kb_proc as unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT, "WH_KEYBOARD_LL"),
        (mask.has_mouse(), &ht.ms, WH_MOUSE_LL, ms_proc as unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT, "WH_MOUSE_LL"),
    ] {
        let have = !cell.get().is_null();
        if have && (!want || reinstall) {
            UnhookWindowsHookEx(cell.get());
            cell.set(std::ptr::null_mut());
        }
        if want && cell.get().is_null() {
            let h = SetWindowsHookExW(id, Some(proc_), hmod, 0);
            if h.is_null() {
                failed.push(what);
            }
            cell.set(h);
        }
    }
    let state = if mask.is_empty() {
        TapState::Idle
    } else if !failed.is_empty() {
        TapState::Unavailable(format!("SetWindowsHookExW({}) failed", failed.join(", ")))
    } else {
        TapState::Active
    };
    ht.hub.update_status(|s| {
        if reinstall {
            s.reenabled += 1;
        }
        s.state = state;
    });
    {
        let mut c = lock(shared);
        c.applied = c.applied.max(gen);
    }
    shared.cv.notify_all();
}

fn finish(ht: &HookThread, t0: Instant) {
    if t0.elapsed() >= HOOK_TIMEOUT_RISK {
        log::warn!("pm-hook: a hook callback took {} ms; reinstalling the hooks", t0.elapsed().as_millis());
        unsafe { PostThreadMessageW(ht.thread_id, WM_REINSTALL, 0, 0) };
    }
}

/// Deliver a dispatch result: `true` = pass the event on.
unsafe fn deliver(out: Vec<Out>) -> bool {
    match out.as_slice() {
        [Out::Original] => return true,
        [] => return false,
        _ => {}
    }
    let mut pass = false;
    let mut inputs: Vec<INPUT> = Vec::new();
    for o in out {
        match o {
            Out::Original => pass = true,
            Out::Synth(Synthetic::Input(i)) => inputs.push(i),
            Out::Synth(_) => {}
        }
    }
    if !inputs.is_empty() {
        let n = SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32);
        if n as usize != inputs.len() {
            log::debug!("pm-hook: SendInput injected {n}/{} events", inputs.len());
        }
    }
    pass
}

unsafe extern "system" fn kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let info = &*(lparam as *const KBDLLHOOKSTRUCT);
    let msg = wparam as u32;
    let kind = match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => EventType::KeyDown,
        WM_KEYUP | WM_SYSKEYUP => EventType::KeyUp,
        _ => return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam),
    };
    let Some(ht) = ctx() else { return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    let tag = if info.flags & LLKHF_INJECTED != 0 { info.dwExtraInfo } else { 0 };
    let t0 = Instant::now();
    let out = {
        let mut ev = Event::new(kind, tag, false, Raw::Key { msg, info });
        ht.hub.dispatch(&mut ev)
    };
    let pass = deliver(out);
    finish(&ht, t0);
    if pass {
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    } else {
        1
    }
}

fn mouse_kind(msg: u32) -> Option<EventType> {
    Some(match msg {
        WM_MOUSEMOVE => EventType::MouseMoved,
        WM_LBUTTONDOWN => EventType::LeftMouseDown,
        WM_LBUTTONUP => EventType::LeftMouseUp,
        WM_RBUTTONDOWN => EventType::RightMouseDown,
        WM_RBUTTONUP => EventType::RightMouseUp,
        WM_MBUTTONDOWN | WM_XBUTTONDOWN => EventType::OtherMouseDown,
        WM_MBUTTONUP | WM_XBUTTONUP => EventType::OtherMouseUp,
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => EventType::ScrollWheel,
        _ => return None,
    })
}

unsafe extern "system" fn ms_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let info = &*(lparam as *const MSLLHOOKSTRUCT);
    let msg = wparam as u32;
    let Some(kind) = mouse_kind(msg) else { return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    let Some(ht) = ctx() else { return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    let tag = if info.flags & LLMHF_INJECTED != 0 { info.dwExtraInfo } else { 0 };
    let t0 = Instant::now();
    let out = {
        let mut ev = Event::new(kind, tag, false, Raw::Mouse { msg, info });
        ht.hub.dispatch(&mut ev)
    };
    let pass = deliver(out);
    finish(&ht, t0);
    if pass {
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// Forwarder
// ---------------------------------------------------------------------------

/// A hook event as seen on the forwarder's thread.
pub enum Forwarded<'a> {
    Key { msg: u32, info: &'a KBDLLHOOKSTRUCT },
    Mouse { msg: u32, info: &'a MSLLHOOKSTRUCT },
}

type FwdHandler = RefCell<Box<dyn FnMut(&Forwarded<'_>) -> bool>>;

/// Runs a hook handler on the thread that owns its state (a GUI thread
/// with a message loop), synchronously, the way that thread's own LL hook
/// used to run there. Create it on that thread; hand [`Forwarder::target`]
/// to the `pm_hook` handler, which calls [`ForwardTarget::call`].
pub struct Forwarder {
    hwnd: HWND,
}

/// The `Send` side of a [`Forwarder`].
#[derive(Clone, Copy, Debug)]
pub struct ForwardTarget {
    hwnd: isize,
}

impl Forwarder {
    /// `handler` returns true to swallow the event.
    pub fn new(handler: impl FnMut(&Forwarded<'_>) -> bool + 'static) -> Option<Forwarder> {
        unsafe {
            let hinst = GetModuleHandleW(std::ptr::null());
            let class: Vec<u16> = "PowerMacHookForwarder\0".encode_utf16().collect();
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(fwd_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
            };
            RegisterClassW(&wc); // fails harmlessly when already registered
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                hinst,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return None;
            }
            let b: Box<FwdHandler> = Box::new(RefCell::new(Box::new(handler)));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(b) as isize);
            Some(Forwarder { hwnd })
        }
    }

    pub fn target(&self) -> ForwardTarget {
        ForwardTarget { hwnd: self.hwnd as isize }
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.hwnd) };
    }
}

impl ForwardTarget {
    /// Run the forwarder's handler for `ev` on its thread and wait (at most
    /// `timeout`) for the answer; true = swallow. A busy or gone thread
    /// counts as "pass".
    pub fn call_with_timeout(&self, ev: &Event<'_>, timeout: Duration) -> bool {
        let fwd = if let Some((msg, info)) = ev.keyboard() {
            Forwarded::Key { msg, info }
        } else if let Some((msg, info)) = ev.mouse() {
            Forwarded::Mouse { msg, info }
        } else {
            return false;
        };
        let mut result: usize = 0;
        let ok = unsafe {
            SendMessageTimeoutW(
                self.hwnd as HWND,
                WM_FORWARD,
                0,
                &fwd as *const Forwarded<'_> as LPARAM,
                SMTO_NORMAL | SMTO_ABORTIFHUNG,
                timeout.as_millis().clamp(1, u32::MAX as u128) as u32,
                &mut result,
            )
        };
        ok != 0 && result == 1
    }

    /// [`Self::call_with_timeout`] with 200 ms.
    pub fn call(&self, ev: &Event<'_>) -> bool {
        self.call_with_timeout(ev, Duration::from_millis(200))
    }
}

unsafe extern "system" fn fwd_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_FORWARD => {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const FwdHandler;
            if p.is_null() || lparam == 0 {
                return 0;
            }
            let fwd = &*(lparam as *const Forwarded<'_>);
            let swallow = match (*p).try_borrow_mut() {
                Ok(mut h) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (h)(fwd))).unwrap_or(false),
                Err(_) => false, // re-entered from inside the handler
            };
            swallow as LRESULT
        }
        WM_NCDESTROY => {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FwdHandler;
            if !p.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(p));
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
