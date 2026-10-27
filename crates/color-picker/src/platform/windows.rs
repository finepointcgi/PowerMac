//! Windows: `RegisterHotKey` on a dedicated thread, and picking with the
//! shared low-level mouse / keyboard hooks (`pm-hook`, at the highest
//! priority while a pick runs, forwarded to the picker thread) plus a
//! magnifier window that follows the cursor (excluded from screen capture so
//! it never magnifies itself).
//!
//! Left click picks, right click or Esc cancels; arrow keys nudge the cursor
//! by one pixel (as in PowerToys).

use std::cell::RefCell;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    FrameRect, GetDC, GetMonitorInfoW, GetPixel, InvalidateRect, MonitorFromPoint, ReleaseDC,
    SelectObject, SetBkMode, SetStretchBltMode, SetTextColor, StretchBlt, COLORONCOLOR, DT_CENTER,
    DT_SINGLELINE, DT_VCENTER, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, SRCCOPY,
    TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
    VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, PostThreadMessageW, RegisterClassW, SetCursorPos, SetLayeredWindowAttributes,
    SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage, HWND_TOPMOST, KBDLLHOOKSTRUCT, LWA_ALPHA, MSG,
    MSLLHOOKSTRUCT, SWP_NOACTIVATE, SWP_NOSIZE, SW_SHOWNOACTIVATE, WDA_EXCLUDEFROMCAPTURE, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_NULL, WM_PAINT, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::{Hotkey, Rgb};

pub type PickCallback = Box<dyn FnOnce(Option<Rgb>) + Send>;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------- hotkeys

/// A hotkey registered on its own message thread; dropping it unregisters.
pub struct HotkeyReg {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl HotkeyReg {
    pub fn register(hk: Hotkey, cb: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        if !hk.is_set() {
            return Ok(Self {
                thread_id: 0,
                thread: None,
            });
        }
        let mut mods = MOD_NOREPEAT;
        if hk.win {
            mods |= MOD_WIN;
        }
        if hk.ctrl {
            mods |= MOD_CONTROL;
        }
        if hk.alt {
            mods |= MOD_ALT;
        }
        if hk.shift {
            mods |= MOD_SHIFT;
        }
        let (tx, rx) = mpsc::channel::<std::result::Result<u32, String>>();
        let code = hk.code;
        let thread = std::thread::Builder::new()
            .name("color-picker-hotkey".into())
            .spawn(move || unsafe {
                let tid = GetCurrentThreadId();
                if RegisterHotKey(std::ptr::null_mut(), 1, mods, code) == 0 {
                    let _ = tx.send(Err("RegisterHotKey failed (in use by another app?)".into()));
                    return;
                }
                let _ = tx.send(Ok(tid));
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    if msg.message == WM_HOTKEY {
                        let cb = cb.clone();
                        std::thread::spawn(move || cb());
                    }
                }
                UnregisterHotKey(std::ptr::null_mut(), 1);
            })?;
        match rx.recv() {
            Ok(Ok(tid)) => Ok(Self {
                thread_id: tid,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                bail!("hotkey {hk}: {e}")
            }
            Err(_) => bail!("hotkey thread died"),
        }
    }
}

impl Drop for HotkeyReg {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------- picking

const ZOOM: i32 = 9;
const CELLS: i32 = 15;
const BOX: i32 = ZOOM * CELLS;
const LABEL: i32 = 30;
const OFFSET: i32 = 24;

struct PickState {
    hwnd: HWND,
    thread: u32,
    cursor: POINT,
    result: Option<Option<Rgb>>,
    /// Swallow the button-up that follows the button-down we consumed.
    swallow_up: bool,
}

thread_local! {
    static STATE: RefCell<Option<PickState>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut PickState) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn color_at(p: POINT) -> Rgb {
    unsafe {
        let dc = GetDC(std::ptr::null_mut());
        let c = GetPixel(dc, p.x, p.y);
        ReleaseDC(std::ptr::null_mut(), dc);
        Rgb::new(
            (c & 0xFF) as u8,
            ((c >> 8) & 0xFF) as u8,
            ((c >> 16) & 0xFF) as u8,
        )
    }
}

fn finish(res: Option<Rgb>) {
    if let Some(tid) = with_state(|s| {
        s.result = Some(res);
        s.thread
    }) {
        unsafe { PostThreadMessageW(tid, WM_NULL, 0, 0) };
    }
}

/// Place the magnifier next to the cursor, flipped away from screen edges.
unsafe fn place(hwnd: HWND, p: POINT) {
    let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(mon, &mut mi);
    let r = mi.rcMonitor;
    let (w, h) = (BOX, BOX + LABEL);
    let mut x = p.x + OFFSET;
    let mut y = p.y + OFFSET;
    if x + w > r.right {
        x = p.x - OFFSET - w;
    }
    if y + h > r.bottom {
        y = p.y - OFFSET - h;
    }
    SetWindowPos(hwnd, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
    InvalidateRect(hwnd, std::ptr::null(), 0);
}

/// The old `WH_MOUSE_LL` body (picker thread, through the forwarder): true = swallow.
unsafe fn mouse_event(msg: u32, info: &MSLLHOOKSTRUCT) -> bool {
    {
        match msg {
            WM_MOUSEMOVE => {
                if let Some(hwnd) = with_state(|s| {
                    s.cursor = info.pt;
                    s.hwnd
                }) {
                    place(hwnd, info.pt);
                }
            }
            WM_LBUTTONDOWN => {
                let c = color_at(info.pt);
                with_state(|s| s.swallow_up = true);
                finish(Some(c));
                return true;
            }
            WM_RBUTTONDOWN => {
                with_state(|s| s.swallow_up = true);
                finish(None);
                return true;
            }
            WM_LBUTTONUP | WM_RBUTTONUP
                if with_state(|s| std::mem::take(&mut s.swallow_up)).unwrap_or(false) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// The old `WH_KEYBOARD_LL` body (picker thread): true = swallow.
unsafe fn key_event(msg: u32, info: &KBDLLHOOKSTRUCT) -> bool {
    if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
        let vk = info.vkCode as u16;
        if vk == VK_ESCAPE {
            finish(None);
            return true;
        }
        if vk == VK_RETURN {
            let mut p: POINT = std::mem::zeroed();
            GetCursorPos(&mut p);
            finish(Some(color_at(p)));
            return true;
        }
        let (dx, dy) = match vk {
            VK_LEFT => (-1, 0),
            VK_RIGHT => (1, 0),
            VK_UP => (0, -1),
            VK_DOWN => (0, 1),
            _ => (0, 0),
        };
        if dx != 0 || dy != 0 {
            let mut p: POINT = std::mem::zeroed();
            GetCursorPos(&mut p);
            SetCursorPos(p.x + dx, p.y + dy);
            return true;
        }
    }
    false
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let dc = BeginPaint(hwnd, &mut ps);
        let p = with_state(|s| s.cursor).unwrap_or(POINT { x: 0, y: 0 });
        // Magnified pixels around the cursor.
        let screen = GetDC(std::ptr::null_mut());
        SetStretchBltMode(dc, COLORONCOLOR);
        StretchBlt(
            dc,
            0,
            0,
            BOX,
            BOX,
            screen,
            p.x - CELLS / 2,
            p.y - CELLS / 2,
            CELLS,
            CELLS,
            SRCCOPY,
        );
        ReleaseDC(std::ptr::null_mut(), screen);
        // Frame and the center pixel.
        let border = CreateSolidBrush(0x00505050);
        let frame = RECT {
            left: 0,
            top: 0,
            right: BOX,
            bottom: BOX + LABEL,
        };
        FrameRect(dc, &frame, border);
        let c0 = (CELLS / 2) * ZOOM;
        let center = RECT {
            left: c0 - 1,
            top: c0 - 1,
            right: c0 + ZOOM + 1,
            bottom: c0 + ZOOM + 1,
        };
        let white = CreateSolidBrush(0x00FFFFFF);
        FrameRect(dc, &center, white);
        // Label: the color and its hex value.
        let c = color_at(p);
        let fill = CreateSolidBrush((c.r as u32) | ((c.g as u32) << 8) | ((c.b as u32) << 16));
        let mut label = RECT {
            left: 1,
            top: BOX,
            right: BOX - 1,
            bottom: BOX + LABEL - 1,
        };
        FillRect(dc, &label, fill);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, if c.is_light() { 0x00000000 } else { 0x00FFFFFF });
        let font = CreateFontW(
            -15,
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            0,
            0,
            0,
            5,
            0,
            wide("Segoe UI").as_ptr(),
        );
        let old = SelectObject(dc, font);
        let mut text: Vec<u16> = c.hex().encode_utf16().collect();
        DrawTextW(
            dc,
            text.as_mut_ptr(),
            text.len() as i32,
            &mut label,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(dc, old);
        DeleteObject(font);
        DeleteObject(fill);
        DeleteObject(white);
        DeleteObject(border);
        EndPaint(hwnd, &ps);
        return 0;
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Pick one color on the current thread (runs its own message loop).
pub fn pick_blocking() -> Option<Rgb> {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("PowerMacColorPickerMagnifier");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc); // fails harmlessly if already registered
        let mut cursor: POINT = std::mem::zeroed();
        GetCursorPos(&mut cursor);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            class.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            cursor.x + OFFSET,
            cursor.y + OFFSET,
            BOX,
            BOX + LABEL,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            log::warn!("color-picker: could not create the magnifier window");
            return None;
        }
        SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
        SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        STATE.with(|s| {
            *s.borrow_mut() = Some(PickState {
                hwnd,
                thread: GetCurrentThreadId(),
                cursor,
                result: None,
                swallow_up: false,
            })
        });
        // Input from the shared hooks (`pm-hook`) at the highest priority
        // (the pick is modal), handled here on the picker thread.
        let fwd = pm_hook::win::Forwarder::new(|f| match f {
            pm_hook::win::Forwarded::Mouse { msg, info } => mouse_event(*msg, info),
            pm_hook::win::Forwarded::Key { msg, info } => key_event(*msg, info),
        });
        let hook = fwd.as_ref().map(|f| {
            use pm_hook::{EventMask, Subscription, Verdict};
            let target = f.target();
            let mask = EventMask::KEYS | EventMask::MOUSE_BUTTONS | EventMask::MOVES;
            pm_hook::subscribe(Subscription::new("color-picker", pm_hook::priority::COLOR_PICKER, mask, move |ev| {
                if target.call(ev) {
                    Verdict::Swallow
                } else {
                    Verdict::Pass
                }
            }))
        });
        place(hwnd, cursor);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let mut msg: MSG = std::mem::zeroed();
        loop {
            if with_state(|s| s.result.is_some()).unwrap_or(true) {
                break;
            }
            if GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) <= 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        drop(hook);
        drop(fwd);
        DestroyWindow(hwnd);
        STATE
            .with(|s| s.borrow_mut().take())
            .and_then(|s| s.result)
            .flatten()
    }
}

/// Start a pick on a worker thread; `cb` runs there when it ends.
pub fn pick_async(cb: PickCallback) {
    std::thread::spawn(move || cb(pick_blocking()));
}
