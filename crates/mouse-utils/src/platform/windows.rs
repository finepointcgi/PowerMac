//! Windows backend.
//!
//! One dedicated thread owns everything:
//! * `WH_MOUSE_LL` + `WH_KEYBOARD_LL` hooks (hotkeys are swallowed);
//! * one layered, topmost, click-through tool window per monitor
//!   (`WS_EX_LAYERED|WS_EX_TRANSPARENT|WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE`),
//!   painted with `UpdateLayeredWindow` from a 32-bit top-down DIB with
//!   per-pixel alpha (drawn by [`crate::raster`]);
//! * a ~60 Hz thread timer (`SetTimer`) that only runs while something is shown.
//!
//! The thread is per-monitor DPI aware, so all coordinates are physical pixels
//! and logical sizes are multiplied by each monitor's scale.
#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::engine::{Button, ModTracker, Point, Scene};
use crate::raster::Canvas;
use crate::{InputMode, Shared};

const WM_APP_KICK: u32 = WM_APP + 1;
const FRAME_MS: u32 = 16;
/// Unassigned VK used to keep the Start menu from opening after a swallowed Win+key.
const VK_DUMMY: u16 = 0xE8;

pub fn double_click_time() -> Duration {
    Duration::from_millis(unsafe { GetDoubleClickTime() } as u64)
}

pub fn run_main_loop_for(dur: Duration) {
    std::thread::sleep(dur);
}

pub fn debug_probe(offsets: &[(f64, f64)]) -> Vec<Option<[u8; 4]>> {
    vec![None; offsets.len()]
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct Overlay {
    hwnd: HWND,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    scale: f64,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
}

impl Overlay {
    unsafe fn destroy(self) {
        SelectObject(self.dc, self.old);
        DeleteObject(self.bmp);
        DeleteDC(self.dc);
        DestroyWindow(self.hwnd);
    }

    unsafe fn present(&self) {
        let pos = POINT { x: self.x, y: self.y };
        let size = SIZE { cx: self.w, cy: self.h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        UpdateLayeredWindow(self.hwnd, std::ptr::null_mut(), &pos, &size, self.dc, &src, 0, &blend, ULW_ALPHA);
    }
}

struct UiState {
    shared: Arc<Shared>,
    class: Vec<u16>,
    overlays: Vec<Overlay>,
    timer: usize,
    mods: ModTracker,
    pressed: [bool; 256],
    last: Option<Scene>,
    visible: bool,
    frames: u64,
}

thread_local! {
    static STATE: RefCell<Option<UiState>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut UiState) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn shared() -> Option<Arc<Shared>> {
    with_state(|s| s.shared.clone())
}

// ------------------------------------------------------------------ monitors

struct MonitorInfo {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    scale: f64,
}

unsafe extern "system" fn enum_monitor(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let v = &mut *(data as *mut Vec<HMONITOR>);
    v.push(hmon);
    1
}

fn monitors() -> Vec<MonitorInfo> {
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(enum_monitor), &mut handles as *mut _ as LPARAM);
    }
    handles
        .into_iter()
        .filter_map(|h| unsafe {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(h, &mut mi) == 0 {
                return None;
            }
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let r = mi.rcMonitor;
            Some(MonitorInfo { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top, scale: dx as f64 / 96.0 })
        })
        .collect()
}

unsafe fn create_overlay(class: &[u16], m: &MonitorInfo) -> Option<Overlay> {
    let hinst = GetModuleHandleW(std::ptr::null());
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
        class.as_ptr(),
        std::ptr::null(),
        WS_POPUP,
        m.x,
        m.y,
        m.w,
        m.h,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        hinst,
        std::ptr::null(),
    );
    if hwnd.is_null() {
        return None;
    }
    let dc = CreateCompatibleDC(std::ptr::null_mut());
    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = m.w;
    bmi.bmiHeader.biHeight = -m.h; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB as _;
    let mut bits: *mut c_void = std::ptr::null_mut();
    let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
    if bmp.is_null() || bits.is_null() {
        DeleteDC(dc);
        DestroyWindow(hwnd);
        return None;
    }
    let old = SelectObject(dc, bmp);
    Some(Overlay { hwnd, x: m.x, y: m.y, w: m.w, h: m.h, scale: m.scale, dc, bmp, old, bits: bits as *mut u32 })
}

// ------------------------------------------------------------------ frames

impl UiState {
    fn sync_monitors(&mut self) {
        let now = monitors();
        let same = now.len() == self.overlays.len()
            && now.iter().zip(&self.overlays).all(|(m, o)| m.x == o.x && m.y == o.y && m.w == o.w && m.h == o.h && m.scale == o.scale);
        if same {
            return;
        }
        unsafe {
            for o in self.overlays.drain(..) {
                o.destroy();
            }
            self.overlays = now.iter().filter_map(|m| create_overlay(&self.class, m)).collect();
        }
        self.visible = false;
        self.last = None;
    }

    fn hide(&mut self) {
        if self.visible {
            for o in &self.overlays {
                unsafe { ShowWindow(o.hwnd, SW_HIDE) };
            }
            self.visible = false;
        }
        self.last = None;
    }

    fn tick(&mut self) {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut pt) };
        let cursor = Point::new(pt.x as f64, pt.y as f64);
        let (scene, keep) = {
            let mut st = self.shared.lock();
            let scene = st.engine.frame(std::time::Instant::now(), cursor);
            let keep = !scene.is_empty() || st.engine.needs_frames();
            if !keep {
                st.timer_running = false;
            }
            (scene, keep)
        };
        if scene.is_empty() {
            self.hide();
        } else {
            if !self.visible || self.frames.is_multiple_of(60) {
                self.sync_monitors();
            }
            self.frames += 1;
            if self.last.as_ref() != Some(&scene) {
                for o in &self.overlays {
                    let px = unsafe { std::slice::from_raw_parts_mut(o.bits, (o.w * o.h) as usize) };
                    let mut c = Canvas::new(o.w as usize, o.h as usize, px);
                    c.draw_scene(&scene, Point::new(o.x as f64, o.y as f64), o.scale);
                    unsafe {
                        GdiFlush();
                        o.present();
                    }
                }
                self.last = Some(scene);
            }
            if !self.visible || self.frames.is_multiple_of(60) {
                for o in &self.overlays {
                    unsafe {
                        ShowWindow(o.hwnd, SW_SHOWNOACTIVATE);
                        SetWindowPos(o.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
                    }
                }
                self.visible = true;
            }
        }
        if !keep {
            self.stop_timer();
        }
    }

    fn start_timer(&mut self) {
        if self.timer == 0 {
            self.timer = unsafe { SetTimer(std::ptr::null_mut(), 0, FRAME_MS, None) };
        }
    }

    fn stop_timer(&mut self) {
        if self.timer != 0 {
            unsafe { KillTimer(std::ptr::null_mut(), self.timer) };
            self.timer = 0;
        }
    }
}

// ------------------------------------------------------------------ hooks

fn is_ours(flags_injected: bool, extra: usize) -> bool {
    flags_injected && extra == pm_input::INJECTED_TAG
}

unsafe extern "system" fn ll_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = &*(lparam as *const MSLLHOOKSTRUCT);
        if !is_ours(info.flags & LLMHF_INJECTED != 0, info.dwExtraInfo) {
            if let Some(sh) = shared() {
                let p = Point::new(info.pt.x as f64, info.pt.y as f64);
                match wparam as u32 {
                    WM_MOUSEMOVE => sh.with(|e, now| e.on_mouse_move(p, None, now)),
                    WM_LBUTTONDOWN => sh.with(|e, now| e.on_button(Button::Left, true, p, now)),
                    WM_LBUTTONUP => sh.with(|e, now| e.on_button(Button::Left, false, p, now)),
                    WM_RBUTTONDOWN => sh.with(|e, now| e.on_button(Button::Right, true, p, now)),
                    WM_RBUTTONUP => sh.with(|e, now| e.on_button(Button::Right, false, p, now)),
                    WM_MBUTTONDOWN | WM_XBUTTONDOWN => sh.with(|e, now| e.dismiss_spotlight(now)),
                    _ => {}
                }
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

fn send_dummy_key() {
    unsafe {
        let mut inputs: [INPUT; 2] = std::mem::zeroed();
        for (i, inp) in inputs.iter_mut().enumerate() {
            inp.r#type = INPUT_KEYBOARD;
            inp.Anonymous.ki = KEYBDINPUT {
                wVk: VK_DUMMY,
                wScan: 0,
                dwFlags: if i == 0 { 0 } else { KEYEVENTF_KEYUP },
                time: 0,
                dwExtraInfo: pm_input::INJECTED_TAG,
            };
        }
        SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn ll_keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
        if !is_ours(kb.flags & LLKHF_INJECTED != 0, kb.dwExtraInfo) {
            let msg = wparam as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let vk = kb.vkCode;
            let info = with_state(|s| {
                let mods = s.mods.mods();
                s.mods.update(vk, down);
                let idx = (vk & 0xFF) as usize;
                let repeat = down && s.pressed[idx];
                s.pressed[idx] = down;
                (s.shared.clone(), mods, repeat)
            });
            if let Some((sh, mods, repeat)) = info {
                let swallow = if repeat {
                    sh.lock().engine.swallowing(vk)
                } else {
                    sh.with(|e, now| e.on_key(vk, down, mods, now))
                };
                if swallow {
                    if down && !repeat && mods.win {
                        send_dummy_key();
                    }
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

unsafe extern "system" fn overlay_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ------------------------------------------------------------------ runtime

pub(crate) struct Runtime {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl Runtime {
    pub(crate) fn start(shared: Arc<Shared>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let thread = std::thread::Builder::new().name("mouse-utils-ui".into()).spawn(move || unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let hinst = GetModuleHandleW(std::ptr::null());
            let class = wide("PowerMacMouseUtilsOverlay");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
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

            // Make sure the thread has a message queue before handing out its id.
            let mut msg: MSG = std::mem::zeroed();
            PeekMessageW(&mut msg, std::ptr::null_mut(), WM_USER, WM_USER, PM_NOREMOVE);
            let tid = GetCurrentThreadId();
            shared.set_kick(move || {
                PostThreadMessageW(tid, WM_APP_KICK, 0, 0);
            });

            STATE.with(|s| {
                *s.borrow_mut() = Some(UiState {
                    shared: shared.clone(),
                    class,
                    overlays: vec![],
                    timer: 0,
                    mods: ModTracker::default(),
                    pressed: [false; 256],
                    last: None,
                    visible: false,
                    frames: 0,
                })
            });

            let h_mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse), hinst, 0);
            let h_kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard), hinst, 0);
            let mode = match (h_mouse.is_null(), h_kb.is_null()) {
                (false, false) => InputMode::Full,
                (false, true) => InputMode::MouseOnly,
                (true, _) => InputMode::None,
            };
            if mode != InputMode::Full {
                log::warn!("mouse-utils: low-level hooks failed (mouse: {}, keyboard: {})", !h_mouse.is_null(), !h_kb.is_null());
            }
            shared.set_input_mode(mode);
            let _ = tx.send(Ok(tid));

            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.hwnd.is_null() {
                    match msg.message {
                        WM_APP_KICK => {
                            with_state(|s| s.start_timer());
                            continue;
                        }
                        WM_TIMER => {
                            with_state(|s| s.tick());
                            continue;
                        }
                        _ => {}
                    }
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            if !h_mouse.is_null() {
                UnhookWindowsHookEx(h_mouse);
            }
            if !h_kb.is_null() {
                UnhookWindowsHookEx(h_kb);
            }
            if let Some(mut st) = STATE.with(|s| s.borrow_mut().take()) {
                st.stop_timer();
                for o in st.overlays.drain(..) {
                    o.destroy();
                }
                st.shared.lock().timer_running = false;
            }
        })?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(thread_id)) => Ok(Runtime { thread_id, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                bail!(e)
            }
            Err(_) => bail!("mouse-utils UI thread did not start"),
        }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            let _ = t.join();
        }
    }
}
