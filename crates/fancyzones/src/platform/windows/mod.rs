//! Windows backend.
//!
//! * Drag detection: `SetWinEventHook(EVENT_SYSTEM_MOVESIZESTART..END)`
//!   (out-of-context) + a 15 ms thread timer polling cursor/Shift/Ctrl, and a
//!   `WH_MOUSE_LL` hook during drags (secondary / middle / X buttons, wheel
//!   layout switching).
//! * New windows: `EVENT_OBJECT_SHOW` (app last zone / open on active monitor).
//! * Display / work area changes: a hidden top-level window receiving
//!   `WM_DISPLAYCHANGE` and `WM_SETTINGCHANGE(SPI_SETWORKAREA)`.
//! * Virtual desktops: the current desktop id is read from the registry
//!   (like PowerToys `VirtualDesktop::GetCurrentVirtualDesktopIdFromRegistry`)
//!   once a second.
//! * Overlay: one layered, click-through, topmost tool window per monitor
//!   (`WS_EX_LAYERED|WS_EX_TRANSPARENT|WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE`)
//!   painted with GDI (color-key background + constant alpha), plus one for
//!   the layout name label.
//! * Hotkeys: `WH_KEYBOARD_LL` hook (Win+Arrow cannot be `RegisterHotKey`ed).
//! * Window placement: `SetWindowPos` compensated with
//!   `DWMWA_EXTENDED_FRAME_BOUNDS` for the invisible resize borders.
//!
//! All hooks, timers and windows live on one dedicated thread that pumps a
//! message loop.
#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Accessibility::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::Modifiers;
use crate::engine::{DragInput, LabelState, Monitor, OverlayState, SwitchFeedback, WindowKey, WindowSystem, WorkArea, FLASH_ZONES_MS};
use crate::filters::{self, AppIdentity, WinStyle, WindowKind};
use crate::geometry::{Point, Rect};
use crate::hotkey_bits::{self, Action};
use crate::keys::vk_digit;
use crate::navigation::Direction;
use crate::{Command, KeyAction, Shared};

const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWCP_DONOTROUND: i32 = 1;
const WM_APP_HOTKEY: u32 = WM_APP + 1;
const WM_APP_WAKE: u32 = WM_APP + 2;
const WM_APP_KEY: u32 = WM_APP + 3;
const WM_APP_WHEEL: u32 = WM_APP + 4;
const WM_APP_MIDDLE: u32 = WM_APP + 5;
const WM_APP_SECONDARY: u32 = WM_APP + 6;
const WM_APP_ROTATION: u32 = WM_APP + 7;
const ROT_HIDE: usize = 0;
const ROT_SHOW: usize = 1;
const ROT_NEXT: usize = 2;
const ROT_PREV: usize = 3;
const COLOR_KEY: COLORREF = 0x00FF00FF; // magenta = transparent
const DRAG_TIMER_MS: u32 = 15;
const LABEL_TICK_MS: u32 = 40;
const POLL_MS: u32 = 1000;
const DISPLAY_TIMER_ID: usize = 1;
const SPI_SETWORKAREA_U: usize = 0x002F;
const WHEEL_DELTA_I: i32 = 120;
/// Unassigned VK used to keep the Start menu from opening after a swallowed Win+key.
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

fn has_style(style: u32, s: u32) -> bool {
    style & s == s
}

fn has_visible_owner(hwnd: HWND) -> bool {
    unsafe {
        let owner = GetWindow(hwnd, GW_OWNER);
        if owner.is_null() || IsWindowVisible(owner) == 0 {
            return false;
        }
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(owner, &mut r) == 0 {
            return true;
        }
        r.top != r.bottom && r.left != r.right
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// `FancyZonesWindowProcessing::DefineWindowType` + `IsExcludedByDefault`.
fn window_kind(hwnd: HWND) -> WindowKind {
    unsafe {
        if hwnd.is_null() || IsWindow(hwnd) == 0 {
            return WindowKind::Never;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == std::process::id() || hwnd == GetDesktopWindow() || hwnd == GetShellWindow() {
            return WindowKind::Never;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let st = WinStyle {
            minimized: IsIconic(hwnd) != 0,
            visible: has_style(style, WS_VISIBLE),
            tool_window: has_style(ex, WS_EX_TOOLWINDOW),
            is_root: GetAncestor(hwnd, GA_ROOT) == hwnd,
            popup: has_style(style, WS_POPUP),
            thick_frame: has_style(style, WS_THICKFRAME),
            caption: has_style(style, WS_CAPTION),
            min_max_box: has_style(style, WS_MINIMIZEBOX) || has_style(style, WS_MAXIMIZEBOX),
            has_visible_owner: false,
        };
        let kind = filters::win_window_kind(WinStyle { has_visible_owner: has_visible_owner(hwnd), ..st });
        if kind == WindowKind::Never {
            return kind;
        }
        let class = class_name(hwnd);
        if ["SysListView32", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd", "Progman", "MsoSplash"].contains(&class.as_str()) {
            return WindowKind::Never;
        }
        if process_path(hwnd).is_some_and(|p| p.to_uppercase().contains("SYSTEMAPPS")) {
            return WindowKind::Never;
        }
        kind
    }
}

fn is_candidate(hwnd: HWND) -> bool {
    window_kind(hwnd) != WindowKind::Never
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

fn window_title(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

unsafe extern "system" fn enum_window(hwnd: HWND, data: LPARAM) -> BOOL {
    let v = &mut *(data as *mut Vec<HWND>);
    v.push(hwnd);
    1
}

fn guid_string(b: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let d2 = u16::from_le_bytes([b[4], b[5]]);
    let d3 = u16::from_le_bytes([b[6], b[7]]);
    format!(
        "{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

fn read_guid(subkey: &str) -> Option<String> {
    let key = wide(subkey);
    let value = wide("CurrentVirtualDesktop");
    let mut buf = [0u8; 16];
    let mut size = buf.len() as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_REG_BINARY, std::ptr::null_mut(), buf.as_mut_ptr() as *mut c_void, &mut size)
    };
    (r == ERROR_SUCCESS && size == 16).then(|| guid_string(&buf))
}

/// Current virtual desktop id (`GetCurrentVirtualDesktopIdFromRegistry`).
pub fn current_virtual_desktop() -> Option<String> {
    read_guid(r"Software\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops").or_else(|| {
        let mut session = 0u32;
        if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } == 0 {
            return None;
        }
        read_guid(&format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo\{session}\VirtualDesktops"))
    })
}

const CORNER_PROP: &str = "PowerMacFancyZones_CornerPreference";

impl WindowSystem for NativeWindowSystem {
    type Window = WinWindow;

    fn monitors(&self) -> Result<Vec<Monitor>> {
        enumerate_monitors()
    }

    fn focused_window(&self) -> Result<Option<WinWindow>> {
        let h = unsafe { GetForegroundWindow() };
        Ok(is_candidate(h).then_some(WinWindow(h as isize)))
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

    fn window_identity(&self, w: &WinWindow) -> AppIdentity {
        AppIdentity { path: process_path(w.hwnd()), title: window_title(w.hwnd()), ..Default::default() }
    }

    fn window_kind(&self, w: &WinWindow) -> WindowKind {
        window_kind(w.hwnd())
    }

    fn window_from_key(&self, key: WindowKey) -> Option<WinWindow> {
        let h = key as isize as HWND;
        (unsafe { IsWindow(h) } != 0).then_some(WinWindow(key as isize))
    }

    /// `FancyZonesWindowUtils::SwitchToWindow`.
    fn focus_window(&self, w: &WinWindow) -> Result<()> {
        unsafe {
            if IsIconic(w.hwnd()) != 0 {
                ShowWindow(w.hwnd(), SW_RESTORE);
            }
            // An (empty) input event lifts the foreground lock for this process.
            let mut input: INPUT = std::mem::zeroed();
            input.r#type = INPUT_MOUSE;
            SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
            if SetForegroundWindow(w.hwnd()) == 0 {
                bail!("SetForegroundWindow failed");
            }
        }
        Ok(())
    }

    fn all_windows(&self) -> Result<Vec<WinWindow>> {
        let mut v: Vec<HWND> = Vec::new();
        unsafe { EnumWindows(Some(enum_window), &mut v as *mut _ as LPARAM) };
        Ok(v.into_iter().filter(|&h| is_candidate(h)).map(|h| WinWindow(h as isize)).collect())
    }

    fn virtual_desktops(&self, monitors: &[Monitor]) -> HashMap<String, String> {
        match current_virtual_desktop() {
            Some(vd) => monitors.iter().map(|m| (m.id.clone(), vd.clone())).collect(),
            None => HashMap::new(),
        }
    }

    /// Windows 11 square corners (`DisableRoundCorners` / `ResetRoundCornersPreference`).
    fn set_snapped_style(&self, w: &WinWindow, snapped: bool) {
        let prop = wide(CORNER_PROP);
        unsafe {
            let h = w.hwnd();
            let saved = GetPropW(h, prop.as_ptr());
            if snapped {
                if saved.is_null() {
                    let mut pref: i32 = 0;
                    DwmGetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &mut pref as *mut i32 as *mut c_void, 4);
                    // high bit distinguishes a saved 0 from "no property"
                    let v = (pref as u32 as u64) | (1u64 << 32);
                    SetPropW(h, prop.as_ptr(), v as usize as HANDLE);
                }
                let v = DWMWCP_DONOTROUND;
                DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &v as *const i32 as *const c_void, 4);
            } else if !saved.is_null() {
                let pref = (saved as usize as u64 & 0xFFFF_FFFF) as i32;
                DwmSetWindowAttribute(h, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const i32 as *const c_void, 4);
                RemovePropW(h, prop.as_ptr());
            }
        }
    }
}

fn cursor_pos() -> Point {
    let mut pt = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut pt) };
    Point::new(pt.x, pt.y)
}

/// Monitor under the mouse cursor.
pub fn monitor_under_cursor() -> Option<String> {
    let _dpi = DpiGuard::new();
    let c = cursor_pos();
    let monitors = enumerate_monitors().ok()?;
    monitors.iter().find(|m| m.frame.contains(c)).or(monitors.first()).map(|m| m.id.clone())
}

// ------------------------------------------------------------------ runtime

/// Saved layered-window state of the dragged window (`WindowMouseSnap::SetWindowTransparency`).
struct Transparency {
    exstyle: i32,
    key: COLORREF,
    alpha: u8,
    flags: u32,
}

struct ActiveDrag {
    window: WinWindow,
    start: Rect,
    areas: Vec<WorkArea>,
    timer: usize,
    transparency: Option<Transparency>,
}

struct OverlayWin {
    hwnd: HWND,
    frame: Rect,
}

struct LabelWin {
    hwnd: HWND,
    state: LabelState,
    rect: Rect,
    font_px: i32,
    shown_at: Instant,
}

struct HookState {
    shared: Arc<Shared>,
    drag: Option<ActiveDrag>,
    overlays: Vec<OverlayWin>,
    overlay: OverlayState,
    class: Vec<u16>,
    label: Option<LabelWin>,
    label_timer: usize,
    flash_timer: usize,
    poll_timer: usize,
    mouse_hook: HHOOK,
    wheel_acc: i32,
    rotation_held: bool,
    rotation_preview: bool,
    last_vd: Option<String>,
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

fn is_modifier_key(vk: u16) -> bool {
    matches!(vk, VK_SHIFT | VK_LSHIFT | VK_RSHIFT | VK_CONTROL | VK_LCONTROL | VK_RCONTROL | VK_MENU | VK_LMENU | VK_RMENU | VK_LWIN | VK_RWIN)
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

fn post(msg: u32, wparam: usize) {
    unsafe { PostThreadMessageW(GetCurrentThreadId(), msg, wparam, 0) };
}

fn key_action_code(a: KeyAction) -> usize {
    match a {
        KeyAction::Editor => 0,
        KeyAction::NextTab => 1,
        KeyAction::PrevTab => 2,
        KeyAction::QuickLayout(k) => 0x10 + k as usize,
    }
}

fn key_action_from(code: usize) -> KeyAction {
    match code {
        0 => KeyAction::Editor,
        1 => KeyAction::NextTab,
        2 => KeyAction::PrevTab,
        c => KeyAction::QuickLayout((c - 0x10) as u8),
    }
}

/// Returns `Some(1)` to swallow the key.
fn handle_key(vk: u16, down: bool, up: bool) -> Option<LRESULT> {
    let (shared, held) = with_state(|s| (s.shared.clone(), s.rotation_held))?;
    let keys = shared.keys();
    let m = current_mods();
    if down {
        if keys.is_rotation_chord(m, |c| c == vk) {
            if !held {
                with_state(|s| s.rotation_held = true);
                post(WM_APP_ROTATION, ROT_SHOW);
                send_dummy_key(); // no menu activation / Start menu on modifier release
            }
            return Some(1);
        }
        if held && (vk == VK_LEFT || vk == VK_RIGHT) {
            post(WM_APP_ROTATION, if vk == VK_LEFT { ROT_PREV } else { ROT_NEXT });
            return Some(1);
        }
        let dir = match vk {
            VK_LEFT => Some(Direction::Left),
            VK_RIGHT => Some(Direction::Right),
            VK_UP => Some(Direction::Up),
            VK_DOWN => Some(Direction::Down),
            _ => None,
        };
        if let Some(dir) = dir {
            let bits = shared.hotkeys.load(Ordering::Relaxed);
            if let Some(a) = hotkey_bits::classify(bits, m, dir.is_horizontal()) {
                // `ShouldProcessSnapHotkey`: leave Windows Snap alone for windows we don't handle.
                let fg = WinWindow(unsafe { GetForegroundWindow() } as isize);
                if !shared.engine().is_processable(&NativeWindowSystem, &fg) {
                    return None;
                }
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
                post(WM_APP_HOTKEY, a_code << 4 | d_code);
                if m.meta {
                    send_dummy_key();
                }
                return Some(1);
            }
        }
        if let Some(a) = keys.classify(m, |c| c == vk, vk_digit(vk)) {
            post(WM_APP_KEY, key_action_code(a));
            if m.meta {
                send_dummy_key();
            }
            return Some(1);
        }
    }
    if up && held {
        let is_activator = keys.rotation.is_some_and(|h| h.code == vk);
        if is_activator || is_modifier_key(vk) {
            with_state(|s| s.rotation_held = false);
            post(WM_APP_ROTATION, ROT_HIDE);
            if is_activator {
                return Some(1);
            }
        }
    }
    None
}

unsafe extern "system" fn ll_keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
        let w = wparam as u32;
        let down = w == WM_KEYDOWN || w == WM_SYSKEYDOWN;
        let up = w == WM_KEYUP || w == WM_SYSKEYUP;
        if kb.flags & LLKHF_INJECTED == 0 {
            if let Some(r) = handle_key(kb.vkCode as u16, down, up) {
                return r;
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

/// Wheel notch(es) during a drag; `true` if consumed (`MouseButtonsHook`).
fn on_wheel(s: &mut HookState, delta: i32) -> bool {
    if !s.shared.zones_shown.load(Ordering::Relaxed) || !s.shared.keys().wheel {
        s.wheel_acc = 0;
        return false;
    }
    if delta != 0 && s.wheel_acc != 0 && (delta < 0) != (s.wheel_acc < 0) {
        s.wheel_acc = 0;
    }
    s.wheel_acc += delta;
    let mut handled = false;
    while s.wheel_acc.abs() >= WHEEL_DELTA_I {
        let up = s.wheel_acc > 0;
        let possible = {
            let e = s.shared.engine();
            let wa = e.drag.as_ref().and_then(|d| d.work_area);
            match (wa, s.drag.as_ref()) {
                (Some(i), Some(d)) => d.areas.get(i).is_some_and(|a| e.next_wheel_layout(&a.monitor.id, up).is_some()),
                _ => false,
            }
        };
        if !possible {
            s.wheel_acc = 0;
            break;
        }
        post(WM_APP_WHEEL, up as usize);
        handled = true;
        s.wheel_acc -= if up { WHEEL_DELTA_I } else { -WHEEL_DELTA_I };
    }
    handled
}

unsafe extern "system" fn ll_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        match wparam as u32 {
            WM_RBUTTONDOWN | WM_XBUTTONDOWN => post(WM_APP_SECONDARY, 0),
            WM_MBUTTONDOWN => post(WM_APP_MIDDLE, 0),
            WM_MOUSEWHEEL => {
                let ms = &*(lparam as *const MSLLHOOKSTRUCT);
                let delta = (ms.mouseData >> 16) as u16 as i16 as i32;
                if with_state(|s| on_wheel(s, delta)).unwrap_or(false) {
                    return 1; // used to switch layouts: don't scroll the window below
                }
            }
            _ => {}
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
        EVENT_OBJECT_DESTROY => on_window_destroyed(hwnd),
        _ => {}
    }
}

fn on_move_start(hwnd: HWND) {
    if !is_candidate(hwnd) {
        return;
    }
    with_state(|s| {
        let ws = NativeWindowSystem::new();
        let w = WinWindow(hwnd as isize);
        let Ok(start) = visible_frame(hwnd) else { return };
        let areas = {
            let mut e = s.shared.engine();
            if !e.is_processable(&ws, &w) {
                return;
            }
            let Ok(areas) = e.areas(&ws) else { return };
            e.drag_start(hwnd as u64, start);
            areas
        };
        let timer = unsafe { SetTimer(std::ptr::null_mut(), 0, DRAG_TIMER_MS, None) };
        if s.mouse_hook.is_null() {
            let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
            s.mouse_hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse), hinst, 0) };
        }
        s.wheel_acc = 0;
        if let Some(mut old) = s.drag.replace(ActiveDrag { window: w, start, areas, timer, transparency: None }) {
            unsafe { KillTimer(std::ptr::null_mut(), old.timer) };
            reset_transparency(&mut old);
        }
    });
    on_drag_tick();
}

fn set_transparency(d: &mut ActiveDrag) {
    if d.transparency.is_some() {
        return;
    }
    unsafe {
        let h = d.window.hwnd();
        let exstyle = GetWindowLongW(h, GWL_EXSTYLE);
        SetWindowLongW(h, GWL_EXSTYLE, exstyle | WS_EX_LAYERED as i32);
        let (mut key, mut alpha, mut flags) = (0u32, 255u8, 0u32);
        if GetLayeredWindowAttributes(h, &mut key, &mut alpha, &mut flags) == 0 {
            (key, alpha, flags) = (0, 255, LWA_ALPHA);
        }
        if SetLayeredWindowAttributes(h, 0, (255 * 50 / 100) as u8, LWA_ALPHA) == 0 {
            SetWindowLongW(h, GWL_EXSTYLE, exstyle);
            return;
        }
        d.transparency = Some(Transparency { exstyle, key, alpha, flags });
    }
}

fn reset_transparency(d: &mut ActiveDrag) {
    if let Some(t) = d.transparency.take() {
        unsafe {
            let h = d.window.hwnd();
            if t.exstyle & WS_EX_LAYERED as i32 != 0 {
                SetLayeredWindowAttributes(h, t.key, t.alpha, t.flags);
            }
            SetWindowLongW(h, GWL_EXSTYLE, t.exstyle);
        }
    }
}

fn end_drag_hooks(s: &mut HookState) {
    if !s.mouse_hook.is_null() {
        unsafe { UnhookWindowsHookEx(s.mouse_hook) };
        s.mouse_hook = std::ptr::null_mut();
    }
    s.wheel_acc = 0;
    s.shared.zones_shown.store(false, Ordering::Relaxed);
}

fn drag_overlay(s: &mut HookState, force: bool) -> Option<OverlayState> {
    let d = s.drag.as_mut()?;
    let m = current_mods();
    let mut e = s.shared.engine();
    if force {
        e.drag_invalidate();
    }
    let o = e.drag_update(&d.areas, DragInput { cursor: cursor_pos(), shift: m.shift, ctrl: m.ctrl });
    let shown = e.drag_zones_shown();
    let transparent = e.config.settings.make_dragged_window_transparent;
    drop(e);
    s.shared.zones_shown.store(shown, Ordering::Relaxed);
    if shown && transparent {
        set_transparency(d);
    } else {
        reset_transparency(d);
    }
    o
}

fn on_drag_tick() {
    let overlay = with_state(|s| {
        let d = s.drag.as_mut()?;
        // A resize (size changed) is not a zone drag.
        if let Ok(now) = visible_frame(d.window.hwnd()) {
            if now.width() != d.start.width() || now.height() != d.start.height() {
                s.shared.engine().drag_cancel();
                let mut d = s.drag.take().unwrap();
                unsafe { KillTimer(std::ptr::null_mut(), d.timer) };
                reset_transparency(&mut d);
                end_drag_hooks(s);
                return Some(OverlayState::default());
            }
        }
        drag_overlay(s, false)
    })
    .flatten();
    if let Some(o) = overlay {
        update_overlay(o);
    }
}

fn on_move_end() {
    let drag = with_state(|s| {
        let d = s.drag.take();
        end_drag_hooks(s);
        d
    })
    .flatten();
    let Some(mut d) = drag else { return };
    unsafe { KillTimer(std::ptr::null_mut(), d.timer) };
    reset_transparency(&mut d);
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

fn on_window_destroyed(hwnd: HWND) {
    let aborted = with_state(|s| {
        let key = hwnd as u64;
        let mut e = s.shared.engine();
        if s.drag.as_ref().is_some_and(|d| d.window.0 == hwnd as isize) {
            // `WM_PRIV_WINDOWDESTROYED`: abort the drag without snapping.
            e.drag_cancel();
            drop(e);
            let d = s.drag.take().unwrap();
            unsafe { KillTimer(std::ptr::null_mut(), d.timer) };
            end_drag_hooks(s);
            return true;
        }
        e.forget_window(key);
        false
    })
    .unwrap_or(false);
    if aborted {
        update_overlay(OverlayState::default());
    }
}

fn on_window_shown(hwnd: HWND) {
    with_state(|s| {
        let ws = NativeWindowSystem::new();
        let mut e = s.shared.engine();
        let st = &e.config.settings;
        if !(st.app_last_zone_move_windows || st.open_window_on_active_monitor) || !is_candidate(hwnd) {
            return;
        }
        match e.on_window_created(&ws, &WinWindow(hwnd as isize), Some(cursor_pos())) {
            Ok(true) => s.shared.persist(&e.config),
            Ok(false) => {}
            Err(err) => log::debug!("fancyzones: new window: {err:#}"),
        }
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

/// After a layout switch during a drag: re-instantiate the drag's work areas.
fn refresh_drag(s: &mut HookState) -> Option<OverlayState> {
    let d = s.drag.as_mut()?;
    if let Ok(a) = s.shared.engine().areas(&NativeWindowSystem) {
        d.areas = a;
    }
    drag_overlay(s, true)
}

fn show_feedback(fb: SwitchFeedback) {
    if let Some(f) = fb.flash {
        update_overlay(f);
        with_state(|s| unsafe {
            if s.flash_timer != 0 {
                KillTimer(std::ptr::null_mut(), s.flash_timer);
            }
            s.flash_timer = SetTimer(std::ptr::null_mut(), 0, FLASH_ZONES_MS as u32, None);
        });
    }
    if let Some(l) = fb.label {
        show_label(l);
    }
}

fn run_and_show(cmd: Command) {
    let r = with_state(|s| {
        let fb = s.shared.run_command(&NativeWindowSystem, cmd, Some(cursor_pos()));
        (fb, refresh_drag(s))
    });
    if let Some((fb, overlay)) = r {
        if let Some(o) = overlay {
            update_overlay(o);
        }
        show_feedback(fb);
    }
}

fn on_key(action: KeyAction) {
    match action {
        KeyAction::Editor => {
            if let Some(shared) = with_state(|s| s.shared.clone()) {
                shared.open_editor(monitor_under_cursor());
            }
        }
        KeyAction::NextTab | KeyAction::PrevTab => {
            with_state(|s| {
                if let Err(err) = s.shared.engine().cycle_windows(&NativeWindowSystem, action == KeyAction::PrevTab) {
                    log::warn!("fancyzones: window switching failed: {err:#}");
                }
            });
        }
        KeyAction::QuickLayout(k) => run_and_show(Command::QuickLayout(k)),
    }
}

fn on_wheel_switch(up: bool) {
    let r = with_state(|s| {
        let d = s.drag.as_ref()?;
        let fb = {
            let mut e = s.shared.engine();
            let fb = e.drag_wheel(&NativeWindowSystem, &d.areas, up);
            if fb.as_ref().is_ok_and(|f| f.is_some()) {
                s.shared.persist(&e.config);
            }
            fb
        };
        match fb {
            Ok(Some(fb)) => Some((fb, refresh_drag(s))),
            Ok(None) => None,
            Err(err) => {
                log::warn!("fancyzones: wheel layout switch failed: {err:#}");
                None
            }
        }
    })
    .flatten();
    if let Some((fb, overlay)) = r {
        if let Some(o) = overlay {
            update_overlay(o);
        }
        show_feedback(fb);
    }
}

fn on_drag_button(middle: bool) {
    let overlay = with_state(|s| {
        s.drag.as_ref()?;
        {
            let mut e = s.shared.engine();
            if middle {
                e.drag_middle_click();
            } else {
                e.drag_toggle_secondary();
            }
        }
        drag_overlay(s, false)
    })
    .flatten();
    if let Some(o) = overlay {
        update_overlay(o);
    }
}

fn on_rotation(code: usize) {
    let overlay = with_state(|s| {
        let ws = NativeWindowSystem::new();
        match code {
            ROT_HIDE => {
                s.rotation_preview = false;
                s.drag.is_none().then(OverlayState::default)
            }
            ROT_SHOW => {
                s.rotation_preview = true;
                if s.drag.is_some() {
                    return None;
                }
                s.shared.engine().rotation_preview(&ws).ok()
            }
            _ => {
                s.shared.run_command(&ws, Command::Rotate(code == ROT_PREV), None);
                (s.rotation_preview && s.drag.is_none()).then(|| s.shared.engine().rotation_preview(&ws).ok()).flatten()
            }
        }
    })
    .flatten();
    if let Some(o) = overlay {
        update_overlay(o);
    }
}

fn on_display_change() {
    with_state(|s| {
        if s.drag.is_some() {
            return;
        }
        let mut e = s.shared.engine();
        match e.on_display_change(&NativeWindowSystem) {
            Ok(n) if n > 0 => log::info!("fancyzones: moved {n} windows back into their zones"),
            Ok(_) => {}
            Err(err) => log::warn!("fancyzones: {err:#}"),
        }
        s.shared.persist(&e.config);
    });
}

fn on_poll() {
    with_state(|s| {
        let vd = current_virtual_desktop();
        if vd != s.last_vd {
            s.last_vd = vd;
            if let Ok(m) = enumerate_monitors() {
                let vds = NativeWindowSystem.virtual_desktops(&m);
                s.shared.engine().set_virtual_desktops(vds, &m);
            }
        }
    });
}

fn on_thread_timer(id: usize) {
    enum T {
        Drag,
        Flash,
        Label,
        Poll,
        Other,
    }
    let t = with_state(|s| {
        if s.drag.as_ref().is_some_and(|d| d.timer == id) {
            T::Drag
        } else if id == s.flash_timer && id != 0 {
            T::Flash
        } else if id == s.label_timer && id != 0 {
            T::Label
        } else if id == s.poll_timer && id != 0 {
            T::Poll
        } else {
            T::Other
        }
    })
    .unwrap_or(T::Other);
    match t {
        T::Drag => on_drag_tick(),
        T::Flash => {
            let hide = with_state(|s| unsafe {
                KillTimer(std::ptr::null_mut(), s.flash_timer);
                s.flash_timer = 0;
                s.drag.is_none() && !s.rotation_preview
            })
            .unwrap_or(false);
            if hide {
                update_overlay(OverlayState::default());
            }
        }
        T::Label => label_tick(),
        T::Poll => on_poll(),
        T::Other => unsafe {
            KillTimer(std::ptr::null_mut(), id);
        },
    }
}

fn colorref(c: (u8, u8, u8)) -> COLORREF {
    let v = (c.0 as u32) | (c.1 as u32) << 8 | (c.2 as u32) << 16;
    if v == COLOR_KEY {
        v ^ 1
    } else {
        v
    }
}

unsafe fn create_layered(class: &[u16], r: Rect) -> HWND {
    CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
        class.as_ptr(),
        std::ptr::null(),
        WS_POPUP,
        r.left,
        r.top,
        r.width(),
        r.height(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        GetModuleHandleW(std::ptr::null()),
        std::ptr::null(),
    )
}

fn update_overlay(state: OverlayState) {
    with_state(|s| unsafe {
        let alpha = (state.opacity.clamp(0.0, 1.0) * 255.0).round().max(40.0) as u8;
        let mut used = vec![false; s.overlays.len()];
        for m in &state.monitors {
            let idx = match s.overlays.iter().position(|o| o.frame == m.frame) {
                Some(i) => i,
                None => {
                    let hwnd = create_layered(&s.class, m.frame);
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

unsafe fn label_font(px: i32) -> HFONT {
    CreateFontW(-px, 0, 0, 0, FW_SEMIBOLD as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, 0, 0, wide("Segoe UI").as_ptr())
}

fn show_label(l: LabelState) {
    with_state(|s| unsafe {
        let _dpi = DpiGuard::new();
        // DIPs -> pixels of the label's monitor
        let c = l.area.center();
        let mon = MonitorFromPoint(POINT { x: c.x, y: c.y }, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let scale = dx as f64 / 96.0;
        let font_px = (l.font_size as f64 * scale).round() as i32;
        let pad = (l.padding as f64 * scale).round() as i32;
        let hdc = GetDC(std::ptr::null_mut());
        let font = label_font(font_px);
        let old = SelectObject(hdc, font);
        let text: Vec<u16> = l.text.encode_utf16().collect();
        let mut tr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        DrawTextW(hdc, text.as_ptr(), text.len() as i32, &mut tr, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(hdc, old);
        DeleteObject(font);
        ReleaseDC(std::ptr::null_mut(), hdc);
        let max_w = (l.area.width() as f64 * 0.96) as i32;
        let w = (tr.right - tr.left + 2 * pad).min(max_w);
        let h = tr.bottom - tr.top + 2 * pad;
        let o = l.chip_origin(w, h);
        let rect = Rect::from_xywh(o.x, o.y, w, h);
        let hwnd = match &s.label {
            Some(lw) => lw.hwnd,
            None => create_layered(&s.class, rect),
        };
        if hwnd.is_null() {
            return;
        }
        s.label = Some(LabelWin { hwnd, state: l, rect, font_px, shown_at: Instant::now() });
        SetLayeredWindowAttributes(hwnd, COLOR_KEY, 255, LWA_COLORKEY | LWA_ALPHA);
        SetWindowPos(hwnd, HWND_TOPMOST, rect.left, rect.top, rect.width(), rect.height(), SWP_NOACTIVATE | SWP_SHOWWINDOW);
        InvalidateRect(hwnd, std::ptr::null(), 1);
        if s.label_timer == 0 {
            s.label_timer = SetTimer(std::ptr::null_mut(), 0, LABEL_TICK_MS, None);
        }
    });
}

/// Fade-out of the layout name label (`LayoutNameFadeOutMillis`).
fn label_tick() {
    with_state(|s| unsafe {
        let Some(lw) = &s.label else {
            KillTimer(std::ptr::null_mut(), s.label_timer);
            s.label_timer = 0;
            return;
        };
        let total = lw.state.duration_ms as f64;
        let fade = (total / 3.0).min(300.0);
        let elapsed = lw.shown_at.elapsed().as_millis() as f64;
        let left = total - elapsed;
        if left <= 0.0 {
            ShowWindow(lw.hwnd, SW_HIDE);
            KillTimer(std::ptr::null_mut(), s.label_timer);
            s.label_timer = 0;
        } else if left < fade {
            SetLayeredWindowAttributes(lw.hwnd, COLOR_KEY, (255.0 * left / fade) as u8, LWA_COLORKEY | LWA_ALPHA);
        }
    });
}

unsafe fn paint_label(hdc: HDC, lw: &LabelWin) {
    let full = RECT { left: 0, top: 0, right: lw.rect.width(), bottom: lw.rect.height() };
    let bg = CreateSolidBrush(COLOR_KEY);
    FillRect(hdc, &full, bg);
    DeleteObject(bg);
    let chip = CreateSolidBrush(colorref(lw.state.background_color));
    let old_brush = SelectObject(hdc, chip);
    let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let radius = 16;
    RoundRect(hdc, 0, 0, full.right + 1, full.bottom + 1, radius, radius);
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_brush);
    DeleteObject(chip);
    let font = label_font(lw.font_px);
    let old_font = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, colorref(lw.state.text_color));
    let text: Vec<u16> = lw.state.text.encode_utf16().collect();
    let mut r = full;
    DrawTextW(hdc, text.as_ptr(), text.len() as i32, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    SelectObject(hdc, old_font);
    DeleteObject(font);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    with_state(|s| {
        if let Some(lw) = s.label.as_ref().filter(|l| l.hwnd == hwnd) {
            paint_label(hdc, lw);
            return;
        }
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

/// Hidden top-level window receiving display / work area broadcasts.
unsafe extern "system" fn msg_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DISPLAYCHANGE => {
            SetTimer(hwnd, DISPLAY_TIMER_ID, 300, None);
            0
        }
        WM_SETTINGCHANGE if wparam == SPI_SETWORKAREA_U => {
            SetTimer(hwnd, DISPLAY_TIMER_ID, 300, None);
            0
        }
        WM_TIMER if wparam == DISPLAY_TIMER_ID => {
            KillTimer(hwnd, DISPLAY_TIMER_ID);
            on_display_change();
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn register_class(name: &[u16], proc_: WNDPROC) {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: proc_,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: GetModuleHandleW(std::ptr::null()),
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: name.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    RegisterClassExW(&wc); // may already exist after a restart; that's fine
}

pub(crate) struct Runtime {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl Runtime {
    pub(crate) fn start(shared: Arc<Shared>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let shared2 = shared.clone();
        let thread = std::thread::Builder::new().name("fancyzones-hooks".into()).spawn(move || unsafe {
            let shared = shared2;
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let hinst = GetModuleHandleW(std::ptr::null());
            let class = wide("PowerMacFancyZonesOverlay");
            register_class(&class, Some(overlay_wndproc));
            let msg_class = wide("PowerMacFancyZonesMessages");
            register_class(&msg_class, Some(msg_wndproc));
            let msg_window = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                msg_class.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinst,
                std::ptr::null(),
            );

            let poll_timer = SetTimer(std::ptr::null_mut(), 0, POLL_MS, None);
            let last_vd = current_virtual_desktop();
            STATE.with(|s| {
                *s.borrow_mut() = Some(HookState {
                    shared: shared.clone(),
                    drag: None,
                    overlays: vec![],
                    overlay: OverlayState::default(),
                    class: class.clone(),
                    label: None,
                    label_timer: 0,
                    flash_timer: 0,
                    poll_timer,
                    mouse_hook: std::ptr::null_mut(),
                    wheel_acc: 0,
                    rotation_held: false,
                    rotation_preview: false,
                    last_vd,
                })
            });
            if let Ok(m) = enumerate_monitors() {
                let vds = NativeWindowSystem.virtual_desktops(&m);
                shared.engine().set_virtual_desktops(vds, &m);
            }

            let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
            let h_move = SetWinEventHook(EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND, std::ptr::null_mut(), Some(win_event), 0, 0, flags);
            let h_show = SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, std::ptr::null_mut(), Some(win_event), 0, 0, flags);
            let h_destroy = SetWinEventHook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY, std::ptr::null_mut(), Some(win_event), 0, 0, flags);
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
            let thread_id = GetCurrentThreadId();
            *shared.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(move || {
                PostThreadMessageW(thread_id, WM_APP_WAKE, 0, 0);
            }));
            let _ = tx.send(Ok(thread_id));

            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.hwnd.is_null() {
                    match msg.message {
                        WM_APP_HOTKEY => {
                            on_hotkey(msg.wParam);
                            continue;
                        }
                        WM_APP_KEY => {
                            on_key(key_action_from(msg.wParam));
                            continue;
                        }
                        WM_APP_WHEEL => {
                            on_wheel_switch(msg.wParam != 0);
                            continue;
                        }
                        WM_APP_MIDDLE | WM_APP_SECONDARY => {
                            on_drag_button(msg.message == WM_APP_MIDDLE);
                            continue;
                        }
                        WM_APP_ROTATION => {
                            on_rotation(msg.wParam);
                            continue;
                        }
                        WM_APP_WAKE => {
                            for cmd in shared.take_commands() {
                                run_and_show(cmd);
                            }
                            continue;
                        }
                        WM_TIMER => {
                            on_thread_timer(msg.wParam);
                            continue;
                        }
                        _ => {}
                    }
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            *shared.waker.lock().unwrap_or_else(|e| e.into_inner()) = None;
            UnhookWinEvent(h_move);
            for h in [h_show, h_destroy] {
                if !h.is_null() {
                    UnhookWinEvent(h);
                }
            }
            if !h_kb.is_null() {
                UnhookWindowsHookEx(h_kb);
            }
            if let Some(mut st) = STATE.with(|s| s.borrow_mut().take()) {
                if let Some(mut d) = st.drag.take() {
                    KillTimer(std::ptr::null_mut(), d.timer);
                    reset_transparency(&mut d);
                }
                if !st.mouse_hook.is_null() {
                    UnhookWindowsHookEx(st.mouse_hook);
                }
                for t in [st.label_timer, st.flash_timer, st.poll_timer] {
                    if t != 0 {
                        KillTimer(std::ptr::null_mut(), t);
                    }
                }
                for o in st.overlays {
                    DestroyWindow(o.hwnd);
                }
                if let Some(l) = st.label {
                    DestroyWindow(l.hwnd);
                }
            }
            if !msg_window.is_null() {
                DestroyWindow(msg_window);
            }
            shared.zones_shown.store(false, Ordering::SeqCst);
        })?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(thread_id)) => Ok(Runtime { thread_id, thread: Some(thread), shared }),
            Ok(Err(e)) => {
                let _ = thread.join();
                bail!(e)
            }
            Err(_) => bail!("fancyzones hook thread did not start"),
        }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            *self.shared.waker.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
