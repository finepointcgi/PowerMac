//! Windows engine: pointer acceleration/speed through `SystemParametersInfo`
//! (session only, restored on stop) and button remapping through a
//! `WH_MOUSE_LL` hook on its own message-loop thread.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::zeroed;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Result};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    SystemParametersInfoW, TranslateMessage, UnhookWindowsHookEx, HHOOK, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT,
    PM_NOREMOVE, SPI_GETMOUSE, SPI_GETMOUSESPEED, SPI_SETMOUSE, SPI_SETMOUSESPEED, WH_MOUSE_LL, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_QUIT, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use crate::actions::Executor;
use crate::{button, windows_speed, ButtonAction, Config, PointerSettings, PM_INPUT_INJECTED_TAG, SELF_INJECTED_TAG};

// ---------------------------------------------------------------------------
// Pointer settings
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WinPointer {
    /// SPI_GETMOUSE: threshold1, threshold2, acceleration (0 = off).
    mouse: [i32; 3],
    /// SPI_GETMOUSESPEED: 1..=20.
    speed: u32,
}

fn read_pointer() -> Option<WinPointer> {
    unsafe {
        let mut mouse = [0i32; 3];
        let mut speed: u32 = 10;
        if SystemParametersInfoW(SPI_GETMOUSE, 0, mouse.as_mut_ptr() as *mut c_void, 0) == 0 {
            return None;
        }
        if SystemParametersInfoW(SPI_GETMOUSESPEED, 0, &mut speed as *mut u32 as *mut c_void, 0) == 0 {
            return None;
        }
        Some(WinPointer { mouse, speed })
    }
}

/// Session-only (fWinIni = 0: not written to the profile, not broadcast).
fn write_pointer(p: WinPointer) {
    unsafe {
        let mut mouse = p.mouse;
        if SystemParametersInfoW(SPI_SETMOUSE, 0, mouse.as_mut_ptr() as *mut c_void, 0) == 0 {
            log::warn!("mouse-tuning: SPI_SETMOUSE failed");
        }
        // SPI_SETMOUSESPEED takes the value itself in pvParam.
        if SystemParametersInfoW(SPI_SETMOUSESPEED, 0, p.speed as usize as *mut c_void, 0) == 0 {
            log::warn!("mouse-tuning: SPI_SETMOUSESPEED failed");
        }
    }
}

/// Desired system values for `s`, given the originals.
fn desired(s: &PointerSettings, orig: WinPointer) -> WinPointer {
    WinPointer {
        mouse: if s.disable_acceleration { [0, 0, 0] } else { orig.mouse },
        speed: s.speed.map(windows_speed).unwrap_or(orig.speed),
    }
}

#[derive(Default)]
struct PointerState {
    orig: Option<WinPointer>,
}

impl PointerState {
    fn apply(&mut self, cfg: &Config) {
        let s = if cfg.enabled { cfg.mouse } else { PointerSettings::default() };
        if s.is_noop() {
            self.restore();
            return;
        }
        if self.orig.is_none() {
            self.orig = read_pointer();
        }
        let Some(orig) = self.orig else {
            log::warn!("mouse-tuning: cannot read the pointer settings");
            return;
        };
        let want = desired(&s, orig);
        if read_pointer() != Some(want) {
            write_pointer(want);
            log::info!("mouse-tuning: pointer {want:?} (was {orig:?})");
        }
    }

    fn restore(&mut self) {
        if let Some(orig) = self.orig.take() {
            if read_pointer() != Some(orig) {
                write_pointer(orig);
                log::info!("mouse-tuning: pointer settings restored");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Button hook
// ---------------------------------------------------------------------------

struct HookCtx {
    table: Arc<Mutex<Vec<ButtonAction>>>,
    actions: mpsc::Sender<ButtonAction>,
    swallowed: u32,
}

thread_local! {
    static CTX: RefCell<Option<HookCtx>> = const { RefCell::new(None) };
}

struct ButtonHook {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl ButtonHook {
    fn start(table: Arc<Mutex<Vec<ButtonAction>>>, actions: mpsc::Sender<ButtonAction>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<std::result::Result<u32, String>>();
        let thread = std::thread::Builder::new().name("mouse-tuning-buttons".into()).spawn(move || unsafe {
            let thread_id = GetCurrentThreadId();
            CTX.with(|c| *c.borrow_mut() = Some(HookCtx { table, actions, swallowed: 0 }));
            let mut msg: MSG = zeroed();
            PeekMessageW(&mut msg, std::ptr::null_mut() as HWND, 0, 0, PM_NOREMOVE);
            let hmod = GetModuleHandleW(std::ptr::null());
            let hook: HHOOK = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0);
            if hook.is_null() {
                let _ = tx.send(Err("SetWindowsHookExW(WH_MOUSE_LL) failed".into()));
                CTX.with(|c| *c.borrow_mut() = None);
                return;
            }
            let _ = tx.send(Ok(thread_id));
            while GetMessageW(&mut msg, std::ptr::null_mut() as HWND, 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            UnhookWindowsHookEx(hook);
            CTX.with(|c| *c.borrow_mut() = None);
        })?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(thread_id)) => Ok(Self { thread_id, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(anyhow!(e))
            }
            Err(_) => Err(anyhow!("button hook thread did not start")),
        }
    }

    fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            let _ = t.join();
        }
    }
}

impl Drop for ButtonHook {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Button number (see [`button`]) and down/up for a hook message.
fn decode(msg: u32, mouse_data: u32) -> Option<(u8, bool)> {
    let x = || match (mouse_data >> 16) as u16 {
        1 => Some(button::BACK),
        2 => Some(button::FORWARD),
        _ => None,
    };
    match msg {
        WM_MBUTTONDOWN => Some((button::MIDDLE, true)),
        WM_MBUTTONUP => Some((button::MIDDLE, false)),
        WM_XBUTTONDOWN => x().map(|b| (b, true)),
        WM_XBUTTONUP => x().map(|b| (b, false)),
        _ => None,
    }
}

fn handle(btn: u8, down: bool) -> bool {
    CTX.with(|c| {
        let mut c = c.borrow_mut();
        let Some(ctx) = c.as_mut() else { return false };
        let bit = 1u32 << btn;
        if down {
            let action = ctx.table.lock().ok().and_then(|t| t.get(btn as usize).cloned()).unwrap_or_default();
            if action == ButtonAction::Default {
                return false;
            }
            ctx.swallowed |= bit;
            if action != ButtonAction::Disabled {
                let _ = ctx.actions.send(action);
            }
            true
        } else {
            let was = ctx.swallowed & bit != 0;
            ctx.swallowed &= !bit;
            was
        }
    })
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam as *const MSLLHOOKSTRUCT);
        let injected = info.flags & LLMHF_INJECTED != 0;
        let ours = injected && (info.dwExtraInfo == PM_INPUT_INJECTED_TAG || info.dwExtraInfo == SELF_INJECTED_TAG);
        if !ours {
            if let Some((btn, down)) = decode(wparam as u32, info.mouseData) {
                if catch_unwind(AssertUnwindSafe(|| handle(btn, down))).unwrap_or(false) {
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub(crate) struct Engine {
    pointer: PointerState,
    table: Arc<Mutex<Vec<ButtonAction>>>,
    executor: Executor,
    hook: Option<ButtonHook>,
}

impl Engine {
    pub(crate) fn start(config: Config) -> Result<Self> {
        let mut e = Self {
            pointer: PointerState::default(),
            table: Arc::new(Mutex::new(config.button_table())),
            executor: Executor::start()?,
            hook: None,
        };
        e.update(config);
        Ok(e)
    }

    pub(crate) fn update(&mut self, config: Config) {
        self.pointer.apply(&config);
        if let Ok(mut t) = self.table.lock() {
            *t = config.button_table();
        }
        match (config.needs_button_hook(), self.hook.is_some()) {
            (true, false) => {
                let r = self
                    .executor
                    .sender()
                    .ok_or_else(|| anyhow!("action executor stopped"))
                    .and_then(|tx| ButtonHook::start(self.table.clone(), tx));
                match r {
                    Ok(h) => self.hook = Some(h),
                    Err(e) => log::warn!("mouse-tuning: {e:#}"),
                }
            }
            (false, true) => {
                if let Some(mut h) = self.hook.take() {
                    h.stop();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn buttons_active(&self) -> bool {
        self.hook.is_some()
    }

    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(mut h) = self.hook.take() {
            h.stop();
        }
        self.pointer.restore();
        self.executor.stop();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
