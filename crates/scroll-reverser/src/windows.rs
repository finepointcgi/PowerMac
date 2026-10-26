//! Windows backend: `WH_MOUSE_LL` hook on a dedicated message-loop thread.
//!
//! Low-level hooks cannot modify events, so a wheel event that must be
//! reversed is swallowed and re-injected (with `SendInput`, from the message
//! loop rather than inside the hook proc) with the negated delta and
//! `dwExtraInfo = SELF_INJECTED_TAG`. Events tagged by us or by `pm-input`
//! pass through untouched.
//!
//! LL hooks carry no device identity, so all wheel input is classified as
//! `Device::Mouse` unless `Config::windows_touchpad_heuristic` is on.

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Result};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, LLMHF_INJECTED, MSG,
    MSLLHOOKSTRUCT, PM_NOREMOVE, WH_MOUSE_LL, WM_APP, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_QUIT,
};

use crate::{
    classify_windows_delta, windows_action, Observer, ScrollEventInfo, SharedConfig, WinAction,
    SELF_INJECTED_TAG,
};

/// Posted to the hook thread: wParam = 1 for horizontal, lParam = new delta.
const WM_REINJECT: u32 = WM_APP + 0x5352;

struct HookCtx {
    shared: Arc<SharedConfig>,
    observer: Option<Observer>,
    thread_id: u32,
}

thread_local! {
    static CTX: RefCell<Option<HookCtx>> = const { RefCell::new(None) };
}

pub(crate) struct Tap {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl Tap {
    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(t) = self.thread.take() {
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            let _ = t.join();
        }
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) fn start(shared: Arc<SharedConfig>, observer: Option<Observer>) -> Result<Tap> {
    let (tx, rx) = mpsc::channel::<std::result::Result<u32, String>>();
    let thread = std::thread::Builder::new()
        .name("scroll-reverser-hook".into())
        .spawn(move || unsafe {
            let thread_id = GetCurrentThreadId();
            CTX.with(|c| *c.borrow_mut() = Some(HookCtx { shared, observer, thread_id }));
            let mut msg: MSG = zeroed();
            // Create the thread's message queue before publishing its id.
            PeekMessageW(&mut msg, std::ptr::null_mut() as HWND, 0, 0, PM_NOREMOVE);

            let hmod = GetModuleHandleW(std::ptr::null());
            let hook: HHOOK = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0);
            if hook.is_null() {
                let _ = tx.send(Err("SetWindowsHookExW(WH_MOUSE_LL) failed".into()));
                return;
            }
            let _ = tx.send(Ok(thread_id));

            while GetMessageW(&mut msg, std::ptr::null_mut() as HWND, 0, 0) > 0 {
                if msg.hwnd.is_null() && msg.message == WM_REINJECT {
                    reinject(msg.wParam == 1, msg.lParam as i32);
                    continue;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            UnhookWindowsHookEx(hook);
            CTX.with(|c| *c.borrow_mut() = None);
        })?;

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(thread_id)) => Ok(Tap { thread_id, thread: Some(thread) }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(anyhow!(e))
        }
        Err(_) => Err(anyhow!("hook thread did not start")),
    }
}

unsafe fn reinject(horizontal: bool, delta: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: delta as u32,
                dwFlags: if horizontal { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL },
                time: 0,
                dwExtraInfo: SELF_INJECTED_TAG,
            },
        },
    };
    if SendInput(1, &input, size_of::<INPUT>() as i32) != 1 {
        // Typically UIPI (elevated window under the cursor) or the secure desktop.
        log::debug!("scroll-reverser: SendInput failed for reversed wheel event");
    }
}

/// Returns true when the event should be swallowed.
fn handle_wheel(horizontal: bool, delta: i32, extra: usize) -> bool {
    CTX.with(|c| {
        let c = c.borrow();
        let Some(ctx) = c.as_ref() else { return false };
        let cfg = ctx.shared.load();
        let action = windows_action(&cfg, horizontal, delta, extra);
        if let Some(obs) = &ctx.observer {
            if extra != SELF_INJECTED_TAG && extra != crate::PM_INPUT_INJECTED_TAG {
                let device = classify_windows_delta(delta, cfg.windows_touchpad_heuristic);
                let rev = cfg.reverse_for(device);
                let (v, h) = if horizontal { (0.0, delta as f64) } else { (delta as f64, 0.0) };
                obs(&ScrollEventInfo {
                    device,
                    vertical: v,
                    horizontal: h,
                    reversed: crate::Reverse {
                        vertical: !horizontal && rev.vertical,
                        horizontal: horizontal && rev.horizontal,
                    },
                    continuous: false,
                    scroll_phase: 0,
                    momentum_phase: 0,
                });
            }
        }
        match action {
            WinAction::Pass => false,
            WinAction::Reinject(d) => unsafe {
                PostThreadMessageW(ctx.thread_id, WM_REINJECT, horizontal as WPARAM, d as LPARAM)
                    != 0
            },
        }
    })
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msg = wparam as u32;
    if code >= 0 && (msg == WM_MOUSEWHEEL || msg == WM_MOUSEHWHEEL) {
        let info = &*(lparam as *const MSLLHOOKSTRUCT);
        let delta = (info.mouseData >> 16) as u16 as i16 as i32;
        let extra = if info.flags & LLMHF_INJECTED != 0 { info.dwExtraInfo } else { 0 };
        let swallow = catch_unwind(AssertUnwindSafe(|| {
            handle_wheel(msg == WM_MOUSEHWHEEL, delta, extra)
        }))
        .unwrap_or(false);
        if swallow {
            return 1;
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}
