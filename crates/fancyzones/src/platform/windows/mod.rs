//! Windows backend.
//!
//! * Drag detection: `SetWinEventHook(EVENT_SYSTEM_MOVESIZESTART..END)`
//!   (out-of-context) + a 15 ms thread timer polling cursor/Shift/Ctrl.
//! * Overlay: one layered, click-through, topmost tool window per monitor
//!   (`WS_EX_LAYERED|WS_EX_TRANSPARENT|WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE`)
//!   painted with GDI (color-key background + constant alpha).
//! * Hotkeys: `WH_KEYBOARD_LL` hook (Win+Arrow cannot be `RegisterHotKey`ed).
//! * Window placement: `SetWindowPos` compensated with
//!   `DWMWA_EXTENDED_FRAME_BOUNDS` for the invisible resize borders.
//!
//! All hooks, the timer and overlay windows live on one dedicated thread
//! that pumps a message loop.
#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::DwmGetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Accessibility::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::Modifiers;
use crate::engine::{DragInput, Monitor, OverlayState, WindowKey, WindowSystem, WorkArea};
use crate::geometry::{Point, Rect};
use crate::hotkey_bits::{self, Action};
use crate::navigation::Direction;
use crate::Shared;

const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
const WM_APP_HOTKEY: u32 = WM_APP + 1;
const COLOR_KEY: COLORREF = 0x00FF00FF; // magenta = transparent
const DRAG_TIMER_MS: u32 = 15;
/// Unassigned VK used to keep the Start menu from opening after a swallowed Win+Arrow.
const VK_DUMMY: u16 = 0xE8;

pub fn has_permissions() -> bool {
    true
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(b: &[u16]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf16_lossy(&b[..end])
}

fn to_rect(r: &RECT) -> Rect {
    Rect::new(r.left, r.top, r.right, r.bottom)
}

/// Temporarily makes the current thread per-monitor DPI aware so all
/// coordinates are physical pixels.
struct DpiGuard(DPI_AWARENESS_CONTEXT);

impl DpiGuard {
    fn new() -> Self {
        DpiGuard(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for DpiGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

/// A top-level window handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WinWindow(pub isize);

impl WinWindow {
    fn hwnd(self) -> HWND {
        self.0 as HWND
    }
}

/// Windows [`WindowSystem`].
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeWindowSystem;

impl NativeWindowSystem {
    pub fn new() -> Self {
        Self
    }
}

unsafe extern "system" fn enum_monitor(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let v = &mut *(data as *mut Vec<HMONITOR>);
    v.push(hmon);
    1
}

fn monitor_device_id(device: &[u16; 32]) -> Option<String> {
    unsafe {
        let mut dd: DISPLAY_DEVICEW = std::mem::zeroed();
        dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
        // EDD_GET_DEVICE_INTERFACE_NAME = 1
        if EnumDisplayDevicesW(device.as_ptr(), 0, &mut dd, 1) != 0 {
            let id = from_wide(&dd.DeviceID);
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
}

pub fn enumerate_monitors() -> Result<Vec<Monitor>> {
    let _dpi = DpiGuard::new();
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(enum_monitor), &mut handles as *mut _ as LPARAM);
    }
    let mut out = Vec::new();
    for h in handles {
        unsafe {
            let mut mi: MONITORINFOEXW = std::mem::zeroed();
            mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(h, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) == 0 {
                continue;
            }
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let device = from_wide(&mi.szDevice);
            let mut id = monitor_device_id(&mi.szDevice).unwrap_or_else(|| device.clone());
            if out.iter().any(|m: &Monitor| m.id == id) {
                id = format!("{id}#{device}");
            }
            out.push(Monitor {
                id,
                name: device,
                frame: to_rect(&mi.monitorInfo.rcMonitor),
                work_area: to_rect(&mi.monitorInfo.rcWork),
                scale: dx as f64 / 96.0,
                primary: mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
    }
    if out.is_empty() {
        bail!("no monitors found");
    }
    Ok(out)
}

fn is_snappable(hwnd: HWND) -> bool {
    unsafe {
        if hwnd.is_null() || IsWindowVisible(hwnd) == 0 || GetAncestor(hwnd, GA_ROOT) != hwnd {
            return false;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW != 0 || style & WS_CHILD != 0 {
            return false;
        }
        if style & WS_CAPTION == 0 && style & WS_THICKFRAME == 0 {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        pid != std::process::id()
    }
}

fn visible_frame(hwnd: HWND) -> Result<Rect> {
    unsafe {
        let mut r: RECT = std::mem::zeroed();
        if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut c_void, std::mem::size_of::<RECT>() as u32) == 0 {
            return Ok(to_rect(&r));
        }
        if GetWindowRect(hwnd, &mut r) == 0 {
            bail!("GetWindowRect failed");
        }
        Ok(to_rect(&r))
    }
}

/// `AdjustRectForSizeWindowToRect` + `SizeWindowToRect`.
fn size_window_to_rect(hwnd: HWND, target: Rect) -> Result<()> {
    unsafe {
        if IsIconic(hwnd) != 0 || IsZoomed(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        for _ in 0..2 {
            // second pass fixes DPI changes when crossing monitors
            let mut wr: RECT = std::mem::zeroed();
            GetWindowRect(hwnd, &mut wr);
            let mut fr: RECT = std::mem::zeroed();
            let mut t = target;
            if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut fr as *mut RECT as *mut c_void, std::mem::size_of::<RECT>() as u32) == 0 {
                t.left -= fr.left - wr.left;
                t.top -= fr.top - wr.top;
                t.right -= fr.right - wr.right;
                t.bottom -= fr.bottom - wr.bottom;
            }
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let mut flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
            if style & WS_THICKFRAME == 0 {
                flags |= SWP_NOSIZE;
            }
            if SetWindowPos(hwnd, std::ptr::null_mut(), t.left, t.top, t.width(), t.height(), flags) == 0 {
                bail!("SetWindowPos failed: {}", GetLastError());
            }
            if visible_frame(hwnd).map(|r| r == target).unwrap_or(true) {
                break;
            }
        }
    }
    Ok(())
}

fn process_path(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

impl WindowSystem for NativeWindowSystem {
    type Window = WinWindow;

    fn monitors(&self) -> Result<Vec<Monitor>> {
        enumerate_monitors()
    }

    fn focused_window(&self) -> Result<Option<WinWindow>> {
        let h = unsafe { GetForegroundWindow() };
        Ok(is_snappable(h).then_some(WinWindow(h as isize)))
    }

    fn window_key(&self, w: &WinWindow) -> WindowKey {
        w.0 as u64
    }

    fn window_rect(&self, w: &WinWindow) -> Result<Rect> {
        let _dpi = DpiGuard::new();
        visible_frame(w.hwnd())
    }

    fn set_window_rect(&self, w: &WinWindow, r: Rect) -> Result<()> {
        let _dpi = DpiGuard::new();
        size_window_to_rect(w.hwnd(), r)
    }

    fn window_app_id(&self, w: &WinWindow) -> Option<String> {
        process_path(w.hwnd())
    }
}

// ------------------------------------------------------------------ runtime

struct ActiveDrag {
    window: WinWindow,
    start: Rect,
    areas: Vec<WorkArea>,
    timer: usize,
    rbutton_down: bool,
}

struct OverlayWin {
    hwnd: HWND,
    frame: Rect,
}

struct HookState {
    shared: Arc<Shared>,
    drag: Option<ActiveDrag>,
    overlays: Vec<OverlayWin>,
    overlay: OverlayState,
    class: Vec<u16>,
    restored: HashSet<isize>,
}

thread_local! {
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut HookState) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

fn current_mods() -> Modifiers {
    Modifiers {
        ctrl: key_down(VK_CONTROL),
        alt: key_down(VK_MENU),
        shift: key_down(VK_SHIFT),
        meta: key_down(VK_LWIN) || key_down(VK_RWIN),
    }
}

fn send_dummy_key() {
    unsafe {
        let mut inputs: [INPUT; 2] = std::mem::zeroed();
        for (i, inp) in inputs.iter_mut().enumerate() {
            inp.r#type = INPUT_KEYBOARD;
            inp.Anonymous.ki = KEYBDINPUT { wVk: VK_DUMMY, wScan: 0, dwFlags: if i == 0 { 0 } else { KEYEVENTF_KEYUP }, time: 0, dwExtraInfo: 0 };
        }
        SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn ll_keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && (wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN) {
        let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
        let dir = match kb.vkCode as u16 {
            VK_LEFT => Some(Direction::Left),
            VK_RIGHT => Some(Direction::Right),
            VK_UP => Some(Direction::Up),
            VK_DOWN => Some(Direction::Down),
            _ => None,
        };
        if let (Some(dir), false) = (dir, kb.flags & LLKHF_INJECTED != 0) {
            let bits = with_state(|s| s.shared.hotkeys.load(Ordering::Relaxed)).unwrap_or(0);
            let m = current_mods();
            if let Some(a) = hotkey_bits::classify(bits, m, dir.is_horizontal()) {
                let a_code = match a {
                    Action::Snap => 0usize,
                    Action::Extend => 1,
                };
                let d_code = match dir {
                    Direction::Left => 0usize,
                    Direction::Right => 1,
                    Direction::Up => 2,
                    Direction::Down => 3,
                };
                PostThreadMessageW(GetCurrentThreadId(), WM_APP_HOTKEY, a_code << 4 | d_code, 0);
                if m.meta {
                    send_dummy_key();
                }
                return 1; // swallow
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

unsafe extern "system" fn win_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, id_child: i32, _: u32, _: u32) {
    if id_object != OBJID_WINDOW || id_child != CHILDID_SELF as i32 {
        return;
    }
    match event {
        EVENT_SYSTEM_MOVESIZESTART => on_move_start(hwnd),
        EVENT_SYSTEM_MOVESIZEEND => on_move_end(),
        EVENT_OBJECT_SHOW => on_window_shown(hwnd),
        _ => {}
    }
}

fn on_move_start(hwnd: HWND) {
    if !is_snappable(hwnd) {
        return;
    }
    with_state(|s| {
        let Ok(start) = visible_frame(hwnd) else { return };
        let Ok(monitors) = enumerate_monitors() else { return };
        let areas = {
            let mut e = s.shared.engine();
            e.drag_start(hwnd as u64, start);
            e.work_areas(&monitors)
        };
        let timer = unsafe { SetTimer(std::ptr::null_mut(), 0, DRAG_TIMER_MS, None) };
        if let Some(old) = s.drag.replace(ActiveDrag { window: WinWindow(hwnd as isize), start, areas, timer, rbutton_down: key_down(VK_RBUTTON) }) {
            unsafe { KillTimer(std::ptr::null_mut(), old.timer) };
        }
    });
    on_drag_tick();
}

fn on_drag_tick() {
    let overlay = with_state(|s| {
        let d = s.drag.as_mut()?;
        // A resize (size changed) is not a zone drag.
        if let Ok(now) = visible_frame(d.window.hwnd()) {
            if now.width() != d.start.width() || now.height() != d.start.height() {
                s.shared.engine().drag_cancel();
                let d = s.drag.take().unwrap();
                unsafe { KillTimer(std::ptr::null_mut(), d.timer) };
                return Some(OverlayState::default());
            }
        }
        let mut pt = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut pt) };
        let rb = key_down(VK_RBUTTON);
        let mut e = s.shared.engine();
        if rb && !d.rbutton_down {
            e.drag_toggle_secondary();
        }
        d.rbutton_down = rb;
        let m = current_mods();
        e.drag_update(&d.areas, DragInput { cursor: Point::new(pt.x, pt.y), shift: m.shift, ctrl: m.ctrl })
    })
    .flatten();
    if let Some(o) = overlay {
        update_overlay(o);
    }
}

fn on_move_end() {
    let drag = with_state(|s| s.drag.take()).flatten();
    let Some(d) = drag else { return };
    unsafe { KillTimer(std::ptr::null_mut(), d.timer) };
    update_overlay(OverlayState::default());
    with_state(|s| {
        let ws = NativeWindowSystem::new();
        let mut e = s.shared.engine();
        if let Err(err) = e.drag_end(&ws, &d.window, &d.areas) {
            log::warn!("fancyzones: snapping failed: {err:#}");
        }
        s.shared.persist(&e.config);
    });
}

fn on_window_shown(hwnd: HWND) {
    with_state(|s| {
        if !s.shared.engine().config.settings.app_last_zone_move_windows || !is_snappable(hwnd) {
            return;
        }
        if !s.restored.insert(hwnd as isize) {
            return;
        }
        let ws = NativeWindowSystem::new();
        let _ = s.shared.engine().restore_last_zone(&ws, &WinWindow(hwnd as isize));
    });
}

fn on_hotkey(code: usize) {
    let action = if code >> 4 == 1 { Action::Extend } else { Action::Snap };
    let dir = match code & 0xF {
        0 => Direction::Left,
        1 => Direction::Right,
        2 => Direction::Up,
        _ => Direction::Down,
    };
    with_state(|s| {
        let ws = NativeWindowSystem::new();
        let mut e = s.shared.engine();
        let r = match action {
            Action::Snap => e.snap_focused(&ws, dir),
            Action::Extend => e.extend_focused(&ws, dir),
        };
        match r {
            Ok(true) => s.shared.persist(&e.config),
            Ok(false) => {}
            Err(err) => log::warn!("fancyzones: keyboard snap failed: {err:#}"),
        }
    });
}

fn colorref(c: (u8, u8, u8)) -> COLORREF {
    let v = (c.0 as u32) | (c.1 as u32) << 8 | (c.2 as u32) << 16;
    if v == COLOR_KEY {
        v ^ 1
    } else {
        v
    }
}

fn update_overlay(state: OverlayState) {
    with_state(|s| unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let alpha = (state.opacity.clamp(0.0, 1.0) * 255.0).round().max(40.0) as u8;
        let mut used = vec![false; s.overlays.len()];
        for m in &state.monitors {
            let idx = match s.overlays.iter().position(|o| o.frame == m.frame) {
                Some(i) => i,
                None => {
                    let hwnd = CreateWindowExW(
                        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                        s.class.as_ptr(),
                        std::ptr::null(),
                        WS_POPUP,
                        m.frame.left,
                        m.frame.top,
                        m.frame.width(),
                        m.frame.height(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        hinst,
                        std::ptr::null(),
                    );
                    if hwnd.is_null() {
                        continue;
                    }
                    s.overlays.push(OverlayWin { hwnd, frame: m.frame });
                    used.push(false);
                    s.overlays.len() - 1
                }
            };
            used[idx] = true;
            let hwnd = s.overlays[idx].hwnd;
            SetLayeredWindowAttributes(hwnd, COLOR_KEY, alpha, LWA_COLORKEY | LWA_ALPHA);
            SetWindowPos(hwnd, HWND_TOPMOST, m.frame.left, m.frame.top, m.frame.width(), m.frame.height(), SWP_NOACTIVATE | SWP_SHOWWINDOW);
            InvalidateRect(hwnd, std::ptr::null(), 1);
        }
        for (i, o) in s.overlays.iter().enumerate() {
            if !used[i] {
                ShowWindow(o.hwnd, SW_HIDE);
            }
        }
        s.overlay = state;
    });
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    with_state(|s| {
        let Some(o) = s.overlays.iter().find(|o| o.hwnd == hwnd) else { return };
        let frame = o.frame;
        let bg = CreateSolidBrush(COLOR_KEY);
        let full = RECT { left: 0, top: 0, right: frame.width(), bottom: frame.height() };
        FillRect(hdc, &full, bg);
        DeleteObject(bg);
        let st = &s.overlay;
        let Some(m) = st.monitors.iter().find(|m| m.frame == frame) else { return };
        let zone_b = CreateSolidBrush(colorref(st.zone_color));
        let hl_b = CreateSolidBrush(colorref(st.highlight_color));
        let border_b = CreateSolidBrush(colorref(st.border_color));
        let font = CreateFontW(-48, 0, 0, 0, FW_BOLD as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, 0, 0, wide("Segoe UI").as_ptr());
        let old_font = SelectObject(hdc, font);
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, colorref(st.number_color));
        for (i, z) in m.zones.iter().enumerate() {
            let mut r = RECT { left: z.left - frame.left, top: z.top - frame.top, right: z.right - frame.left, bottom: z.bottom - frame.top };
            let hl = m.highlighted.contains(&i);
            FillRect(hdc, &r, if hl { hl_b } else { zone_b });
            for k in 0..2 {
                let fr = RECT { left: r.left + k, top: r.top + k, right: r.right - k, bottom: r.bottom - k };
                FrameRect(hdc, &fr, if hl { hl_b } else { border_b });
            }
            if st.show_numbers {
                let txt: Vec<u16> = (i + 1).to_string().encode_utf16().collect();
                DrawTextW(hdc, txt.as_ptr(), txt.len() as i32, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            }
        }
        SelectObject(hdc, old_font);
        DeleteObject(font);
        DeleteObject(zone_b);
        DeleteObject(hl_b);
        DeleteObject(border_b);
    });
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn overlay_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub(crate) struct Runtime {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl Runtime {
    pub(crate) fn start(shared: Arc<Shared>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let thread = std::thread::Builder::new().name("fancyzones-hooks".into()).spawn(move || unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let hinst = GetModuleHandleW(std::ptr::null());
            let class = wide("PowerMacFancyZonesOverlay");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(overlay_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&wc); // may already exist after a restart; that's fine

            STATE.with(|s| {
                *s.borrow_mut() = Some(HookState {
                    shared,
                    drag: None,
                    overlays: vec![],
                    overlay: OverlayState::default(),
                    class: class.clone(),
                    restored: HashSet::new(),
                })
            });

            let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
            let h_move = SetWinEventHook(EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND, std::ptr::null_mut(), Some(win_event), 0, 0, flags);
            let h_show = SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, std::ptr::null_mut(), Some(win_event), 0, 0, flags);
            let h_kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard), hinst, 0);
            if h_move.is_null() {
                let _ = tx.send(Err("SetWinEventHook failed".into()));
                return;
            }
            if h_kb.is_null() {
                log::warn!("fancyzones: WH_KEYBOARD_LL hook failed; keyboard snapping disabled");
            }
            // Make sure the thread has a message queue before reporting ready.
            let mut msg: MSG = std::mem::zeroed();
            PeekMessageW(&mut msg, std::ptr::null_mut(), WM_USER, WM_USER, PM_NOREMOVE);
            let _ = tx.send(Ok(GetCurrentThreadId()));

            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.hwnd.is_null() {
                    match msg.message {
                        WM_APP_HOTKEY => {
                            on_hotkey(msg.wParam);
                            continue;
                        }
                        WM_TIMER => {
                            on_drag_tick();
                            continue;
                        }
                        _ => {}
                    }
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            UnhookWinEvent(h_move);
            if !h_show.is_null() {
                UnhookWinEvent(h_show);
            }
            if !h_kb.is_null() {
                UnhookWindowsHookEx(h_kb);
            }
            if let Some(st) = STATE.with(|s| s.borrow_mut().take()) {
                if let Some(d) = st.drag {
                    KillTimer(std::ptr::null_mut(), d.timer);
                }
                for o in st.overlays {
                    DestroyWindow(o.hwnd);
                }
            }
        })?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(thread_id)) => Ok(Runtime { thread_id, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                bail!(e)
            }
            Err(_) => bail!("fancyzones hook thread did not start"),
        }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            let _ = t.join();
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}
