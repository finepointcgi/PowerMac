//! Windows backend: capture through the shared `WH_MOUSE_LL` / `WH_KEYBOARD_LL`
//! hooks (`pm-hook`), `SendInput` injection.
//!
//! Coordinates are physical pixels in the virtual-desktop space (origin at the
//! primary monitor's top-left; other monitors may have negative coordinates).
//! `WinBackend::new` makes the process Per-Monitor-v2 DPI aware so that
//! `GetCursorPos`, hook coordinates and monitor rects are all un-virtualized.
//!
//! Hook notes:
//! - Low-level hooks run on the thread that installed them, which must pump
//!   messages: that is `pm-hook`'s thread; this crate only subscribes.
//! - The callback must return quickly: if it exceeds `LowLevelHooksTimeout`
//!   (~1s on Win10+) Windows silently removes the hook.
//! - Swallowing `WM_MOUSEMOVE` in the hook freezes the real cursor, which is what
//!   MWB relies on while controlling another machine.
//! - Mouse deltas: `MSLLHOOKSTRUCT.pt` is where the cursor *would* go. We compute
//!   `dx/dy = pt - GetCursorPos()` (the cursor has not moved yet when the hook
//!   runs), which stays correct when moves are swallowed. Windows clamps `pt` to
//!   the virtual desktop, so when the cursor is pinned against an outer edge the
//!   outward delta is 0 — detect edge crossing from the position (as MWB does),
//!   or keep the cursor away from the edge (park it) while forwarding.

#![allow(clippy::upper_case_acronyms)]

use std::mem::{size_of, zeroed};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use windows_sys::Win32::Foundation::{BOOL, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateCursor, GetCursorPos, GetSystemMetrics, SetCursorPos, SetSystemCursor,
    SystemParametersInfoW, KBDLLHOOKSTRUCT,
    LLKHF_EXTENDED, LLKHF_UP, MSLLHOOKSTRUCT,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SPI_SETCURSORS,
    SYSTEM_CURSOR_ID, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP,
};

use crate::{
    CaptureCallback, CaptureHandle, Disposition, InputBackend, InputEvent, KeyEvent, MouseButton,
    MouseEvent, Rect, INJECTED_TAG,
};

const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

// Standard system cursor ids (OCR_*), all replaced while the cursor is hidden.
const SYSTEM_CURSORS: [SYSTEM_CURSOR_ID; 14] = [
    32512, // OCR_NORMAL
    32513, // OCR_IBEAM
    32514, // OCR_WAIT
    32515, // OCR_CROSS
    32516, // OCR_UP
    32642, // OCR_SIZENWSE
    32643, // OCR_SIZENESW
    32644, // OCR_SIZEWE
    32645, // OCR_SIZENS
    32646, // OCR_SIZEALL
    32648, // OCR_NO
    32649, // OCR_HAND
    32650, // OCR_APPSTARTING
    32651, // OCR_HELP
];

pub struct WinBackend {
    cursor_hidden: Mutex<bool>,
}

impl WinBackend {
    pub fn new() -> Result<Self> {
        // Fails harmlessly if awareness was already set (e.g. by the manifest).
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Ok(Self { cursor_hidden: Mutex::new(false) })
    }

    fn send(inputs: &[INPUT]) -> Result<()> {
        let n = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
        if n as usize != inputs.len() {
            // Typically UIPI: target is elevated, or the secure desktop is active.
            bail!("SendInput injected {n}/{} events", inputs.len());
        }
        Ok(())
    }

    fn mouse_input(dx: i32, dy: i32, data: u32, flags: u32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INJECTED_TAG,
                },
            },
        }
    }

    /// Map a virtual-desktop pixel to SendInput's normalized 0..=65535 space.
    fn normalize(x: i32, y: i32) -> (i32, i32) {
        let (vx, vy, vw, vh) = virtual_screen();
        let nx = ((x - vx) as i64 * 65535 / (vw - 1).max(1) as i64) as i32;
        let ny = ((y - vy) as i64 * 65535 / (vh - 1).max(1) as i64) as i32;
        (nx, ny)
    }
}

fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

fn restore_system_cursors() {
    unsafe { SystemParametersInfoW(SPI_SETCURSORS, 0, std::ptr::null_mut(), 0) };
}

impl Drop for WinBackend {
    fn drop(&mut self) {
        if let Ok(h) = self.cursor_hidden.lock() {
            if *h {
                restore_system_cursors();
            }
        }
    }
}

impl InputBackend for WinBackend {
    fn start_capture(&self, cb: CaptureCallback) -> Result<Box<dyn CaptureHandle>> {
        start_hooks(cb)
    }

    fn inject_mouse(&self, ev: MouseEvent) -> Result<()> {
        let abs = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
        let input = match ev {
            MouseEvent::Move { x, y, .. } => {
                let (nx, ny) = Self::normalize(x, y);
                Self::mouse_input(nx, ny, 0, abs)
            }
            MouseEvent::Button { button, down, x, y } => {
                let (nx, ny) = Self::normalize(x, y);
                let (flag, data) = match (button, down) {
                    (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (MouseButton::X1, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
                    (MouseButton::X1, false) => (MOUSEEVENTF_XUP, XBUTTON1),
                    (MouseButton::X2, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
                    (MouseButton::X2, false) => (MOUSEEVENTF_XUP, XBUTTON2),
                };
                // Windows synthesizes double clicks itself from timing/position.
                Self::mouse_input(nx, ny, data, flag | abs)
            }
            MouseEvent::Wheel { delta } => Self::mouse_input(0, 0, delta as u32, MOUSEEVENTF_WHEEL),
            MouseEvent::HWheel { delta } => {
                Self::mouse_input(0, 0, delta as u32, MOUSEEVENTF_HWHEEL)
            }
        };
        Self::send(&[input])
    }

    fn inject_key(&self, ev: KeyEvent) -> Result<()> {
        let scan = unsafe { MapVirtualKeyW(ev.vk, MAPVK_VK_TO_VSC) } as u16;
        let mut flags = 0;
        if ev.extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !ev.down {
            flags |= KEYEVENTF_KEYUP;
        }
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: ev.vk as u16,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INJECTED_TAG,
                },
            },
        };
        Self::send(&[input])
    }

    fn set_cursor_pos(&self, x: i32, y: i32) -> Result<()> {
        if unsafe { SetCursorPos(x, y) } == 0 {
            bail!("SetCursorPos failed");
        }
        Ok(())
    }

    fn cursor_pos(&self) -> Result<(i32, i32)> {
        let mut p = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut p) } == 0 {
            bail!("GetCursorPos failed");
        }
        Ok((p.x, p.y))
    }

    /// `ShowCursor` only affects windows of the calling thread, so it cannot hide
    /// the cursor globally. MWB itself never truly hides it: it parks the cursor
    /// (screen centre, or y=4 with "hide mouse" on) and draws a fake cursor.
    /// We instead replace every standard system cursor with a blank 32x32 one via
    /// `SetSystemCursor`, and restore them with
    /// `SystemParametersInfoW(SPI_SETCURSORS)` (which reloads the user's scheme).
    /// Caveat: if the process is killed while hidden, the cursor stays blank
    /// until the next `SPI_SETCURSORS`, sign-out, or cursor-scheme change.
    /// Apps that set a custom (non-system) cursor still show it.
    fn set_cursor_visible(&self, visible: bool) -> Result<()> {
        let mut hidden = self.cursor_hidden.lock().map_err(|_| anyhow!("poisoned"))?;
        if visible {
            if *hidden {
                restore_system_cursors();
                *hidden = false;
            }
            return Ok(());
        }
        if *hidden {
            return Ok(());
        }
        let and_mask = [0xFFu8; 32 * 32 / 8];
        let xor_mask = [0u8; 32 * 32 / 8];
        let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
        for id in SYSTEM_CURSORS {
            unsafe {
                let cur = CreateCursor(
                    hinst,
                    0,
                    0,
                    32,
                    32,
                    and_mask.as_ptr().cast(),
                    xor_mask.as_ptr().cast(),
                );
                if cur.is_null() {
                    continue;
                }
                // SetSystemCursor takes ownership of (and destroys) `cur`.
                SetSystemCursor(cur, id);
            }
        }
        *hidden = true;
        Ok(())
    }

    fn screens(&self) -> Result<Vec<Rect>> {
        unsafe extern "system" fn enum_proc(
            mon: HMONITOR,
            _hdc: HDC,
            _rc: *mut RECT,
            data: LPARAM,
        ) -> BOOL {
            let out = &mut *(data as *mut Vec<(bool, Rect)>);
            let mut mi: MONITORINFO = zeroed();
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) != 0 {
                let r = mi.rcMonitor;
                out.push((
                    mi.dwFlags & 1 != 0, // MONITORINFOF_PRIMARY
                    Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom },
                ));
            }
            1
        }
        let mut v: Vec<(bool, Rect)> = Vec::new();
        let ok = unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(enum_proc),
                &mut v as *mut _ as LPARAM,
            )
        };
        if ok == 0 || v.is_empty() {
            bail!("EnumDisplayMonitors failed");
        }
        v.sort_by_key(|(primary, _)| !*primary);
        Ok(v.into_iter().map(|(_, r)| r).collect())
    }

    fn has_permissions(&self, _prompt: bool) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Capture (a pm-hook subscriber)
// ---------------------------------------------------------------------------

/// Keeps the subscription to the shared hooks alive.
struct WinCapture {
    _guard: pm_hook::Guard,
}

impl CaptureHandle for WinCapture {}

fn start_hooks(cb: CaptureCallback) -> Result<Box<dyn CaptureHandle>> {
    use pm_hook::{EventMask, Subscription, Verdict};
    let mask = EventMask::KEYS | EventMask::MOUSE_BUTTONS | EventMask::MOVES | EventMask::SCROLL;
    let guard = pm_hook::subscribe(
        Subscription::new("mouse-without-borders", pm_hook::priority::MOUSE_WITHOUT_BORDERS, mask, move |ev| {
            let disp = if let Some((msg, info)) = ev.mouse() {
                on_mouse(&cb, msg, info)
            } else if let Some((_, info)) = ev.keyboard() {
                on_key(&cb, info)
            } else {
                Disposition::Pass
            };
            match disp {
                Disposition::Pass => Verdict::Pass,
                Disposition::Swallow => Verdict::Swallow,
            }
        })
        // Our own injections (LLxHF_INJECTED + INJECTED_TAG) are never reported.
        .ignore_tags(&[INJECTED_TAG]),
    );
    let st = pm_hook::status();
    if !st.is_active() {
        drop(guard);
        return Err(anyhow!("SetWindowsHookExW failed ({:?})", st.state));
    }
    Ok(Box::new(WinCapture { _guard: guard }))
}

fn dispatch(cb: &CaptureCallback, ev: InputEvent) -> Disposition {
    catch_unwind(AssertUnwindSafe(|| cb(&ev))).unwrap_or_else(|_| {
        log::error!("pm-input: capture callback panicked");
        Disposition::Pass
    })
}

fn on_mouse(cb: &CaptureCallback, msg: u32, info: &MSLLHOOKSTRUCT) -> Disposition {
    let (x, y) = (info.pt.x, info.pt.y);
    let hi = (info.mouseData >> 16) as u16;
    let ev = match msg {
        WM_MOUSEMOVE => {
            let mut cur = POINT { x, y };
            unsafe { GetCursorPos(&mut cur) };
            Some(MouseEvent::Move { x, y, dx: x - cur.x, dy: y - cur.y })
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP => Some(MouseEvent::Button {
            button: MouseButton::Left,
            down: msg == WM_LBUTTONDOWN,
            x,
            y,
        }),
        WM_RBUTTONDOWN | WM_RBUTTONUP => Some(MouseEvent::Button {
            button: MouseButton::Right,
            down: msg == WM_RBUTTONDOWN,
            x,
            y,
        }),
        WM_MBUTTONDOWN | WM_MBUTTONUP => Some(MouseEvent::Button {
            button: MouseButton::Middle,
            down: msg == WM_MBUTTONDOWN,
            x,
            y,
        }),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let button = if hi as u32 == XBUTTON2 { MouseButton::X2 } else { MouseButton::X1 };
            Some(MouseEvent::Button { button, down: msg == WM_XBUTTONDOWN, x, y })
        }
        WM_MOUSEWHEEL => Some(MouseEvent::Wheel { delta: hi as i16 as i32 }),
        WM_MOUSEHWHEEL => Some(MouseEvent::HWheel { delta: hi as i16 as i32 }),
        _ => None,
    };
    match ev {
        Some(ev) => dispatch(cb, InputEvent::Mouse(ev)),
        None => Disposition::Pass,
    }
}

fn on_key(cb: &CaptureCallback, info: &KBDLLHOOKSTRUCT) -> Disposition {
    let ev = KeyEvent {
        vk: info.vkCode,
        down: info.flags & LLKHF_UP == 0,
        extended: info.flags & LLKHF_EXTENDED != 0,
    };
    dispatch(cb, InputEvent::Key(ev))
}
