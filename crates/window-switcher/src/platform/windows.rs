//! Windows backend (compile-checked; not yet run on Windows).
//!
//! One dedicated thread owns everything: `WH_KEYBOARD_LL` + `WH_MOUSE_LL`
//! hooks, an `EVENT_SYSTEM_FOREGROUND` WinEvent hook for most-recently-used
//! order, and the overlay — a topmost, non-activating tool window painted
//! with GDI (dark rounded panel, accent selection, icons and titles) with
//! live DWM thumbnails (`DwmRegisterThumbnail`) on top. Other threads talk
//! to it through a channel + `PostThreadMessageW`.
//!
//! Windows' own Alt+Tab already switches windows, so the default shortcut
//! is only Alt+` (this app's windows); `replace_cmd_tab` takes over Alt+Tab.

#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{anyhow, Result};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Config, Mods};
use crate::engine::{Engine, Snapshot};
use crate::model::{self, FilterContext, Mode, Mru, RectF, WindowInfo};
use crate::switcher::{Effect, WindowAction};

enum Cmd {
    Update(Config),
    Show(Mode),
    Hide,
    Stop,
}

static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const WM_CMD: u32 = WM_APP + 1;
const TIMER_SHOW: usize = 1;
/// Unassigned VK, injected so a swallowed Alt chord doesn't open menus.
const VK_DUMMY: u16 = 0xE8;

struct Ctx {
    engine: Engine,
    mru: Mru,
    overlay: HWND,
    thumbs: Vec<isize>,
    mods: Mods,
    swallow_mouse_up: bool,
    shown: bool,
    scale: f64,
    font: HFONT,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ------------------------------------------------------------ enumeration

unsafe fn exe_path(pid: u32) -> String {
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if h.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 1024];
    let mut n = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut n);
    CloseHandle(h);
    if ok == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe fn cloaked(hwnd: HWND) -> u32 {
    let mut v: u32 = 0;
    let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, &mut v as *mut u32 as *mut _, 4);
    v
}

/// Alt+Tab-style eligibility (visible, unowned or app window, not a tool
/// window, has a title), in z-order.
unsafe fn enumerate() -> Vec<WindowInfo> {
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam as *mut Vec<WindowInfo>);
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let owner = GetWindow(hwnd, GW_OWNER);
        let app_window = ex & WS_EX_APPWINDOW != 0;
        if (ex & WS_EX_TOOLWINDOW != 0 && !app_window) || (!owner.is_null() && !app_window) || ex & WS_EX_NOACTIVATE != 0 {
            return 1;
        }
        let title = window_text(hwnd);
        if title.is_empty() {
            return 1;
        }
        let c = cloaked(hwnd);
        // Shell-cloaked = on another virtual desktop; other cloaking = hidden UWP frames etc.
        if c != 0 && c != DWM_CLOAKED_SHELL {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let path = exe_path(pid);
        let name = path.rsplit('\\').next().unwrap_or("").trim_end_matches(".exe").to_string();
        let mut r: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut r);
        let minimized = IsIconic(hwnd) != 0;
        if minimized {
            // Use the restored size for the tile's aspect ratio.
            let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
            wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
            if GetWindowPlacement(hwnd, &mut wp) != 0 {
                r = wp.rcNormalPosition;
            }
        }
        out.push(WindowInfo {
            id: hwnd as usize as u64,
            pid: pid as i64,
            app_name: name,
            app_id: path.clone(),
            app_path: path,
            title,
            frame: RectF::new(r.left as f64, r.top as f64, (r.right - r.left) as f64, (r.bottom - r.top) as f64),
            minimized,
            app_hidden: false,
            other_space: c == DWM_CLOAKED_SHELL,
            fullscreen: false,
            own: pid == GetCurrentProcessId(),
        });
        1
    }
    let mut out: Vec<WindowInfo> = Vec::new();
    EnumWindows(Some(cb), &mut out as *mut _ as LPARAM);
    out
}

unsafe fn screens() -> Vec<RectF> {
    unsafe extern "system" fn cb(_m: HMONITOR, _dc: HDC, r: *mut RECT, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam as *mut Vec<RectF>);
        let r = &*r;
        out.push(RectF::new(r.left as f64, r.top as f64, (r.right - r.left) as f64, (r.bottom - r.top) as f64));
        1
    }
    let mut out: Vec<RectF> = Vec::new();
    EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(cb), &mut out as *mut _ as LPARAM);
    out
}

/// Layout works in points; convert pixel rects with the system scale.
fn to_points(r: RectF, s: f64) -> RectF {
    RectF::new(r.x / s, r.y / s, r.w / s, r.h / s)
}

fn snapshot(ctx_mru: &Mru, cfg: &Config, mode: Mode, scale: f64) -> Snapshot {
    unsafe {
        let mut all = enumerate();
        ctx_mru.sort(&mut all);
        let fg = GetForegroundWindow();
        let mut fg_pid = 0u32;
        GetWindowThreadProcessId(fg, &mut fg_pid);
        if let Some(k) = all.iter().position(|w| w.id == fg as usize as u64) {
            let w = all.remove(k);
            all.insert(0, w);
        }
        let scr = screens();
        let mut pt: POINT = std::mem::zeroed();
        GetCursorPos(&mut pt);
        let cur = scr.iter().position(|s| s.contains(pt.x as f64, pt.y as f64)).or(if scr.is_empty() { None } else { Some(0) });
        let ctx = FilterContext { active_pid: (fg_pid != 0).then_some(fg_pid as i64), screens: scr.clone(), current_screen: cur };
        let mut items = model::filter(all, cfg, mode, &ctx);
        // Frames in points so aspect ratios and the layout agree.
        for w in &mut items {
            w.frame = to_points(w.frame, scale);
        }
        let screen = cur.and_then(|i| scr.get(i).copied()).map(|s| to_points(s, scale)).unwrap_or(RectF::new(0.0, 0.0, 1280.0, 720.0));
        Snapshot { items, screen, icon_mode: !cfg.thumbnails }
    }
}

// ------------------------------------------------------------ effects

unsafe fn inject_dummy() {
    let mk = |flags: u32| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VK_DUMMY, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: pm_input::INJECTED_TAG },
        },
    };
    let inputs = [mk(0), mk(KEYEVENTF_KEYUP)];
    SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
}

unsafe fn focus(w: &WindowInfo) {
    let hwnd = w.id as usize as HWND;
    if IsIconic(hwnd) != 0 {
        ShowWindow(hwnd, SW_RESTORE);
    }
    // Being the source of the last input lets SetForegroundWindow succeed.
    inject_dummy();
    SetForegroundWindow(hwnd);
}

unsafe fn act(a: WindowAction, w: &WindowInfo) {
    let hwnd = w.id as usize as HWND;
    match a {
        WindowAction::Close => {
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
        }
        WindowAction::ToggleMinimize => {
            ShowWindow(hwnd, if IsIconic(hwnd) != 0 { SW_RESTORE } else { SW_MINIMIZE });
        }
        WindowAction::ToggleFullScreen => {
            ShowWindow(hwnd, if IsZoomed(hwnd) != 0 { SW_RESTORE } else { SW_MAXIMIZE });
        }
        // Windows has no app hiding: minimize all of the app's windows.
        WindowAction::ToggleHide | WindowAction::QuitApp => {
            for o in enumerate().into_iter().filter(|o| o.pid == w.pid) {
                let h = o.id as usize as HWND;
                if a == WindowAction::QuitApp {
                    PostMessageW(h, WM_CLOSE, 0, 0);
                } else {
                    ShowWindow(h, SW_MINIMIZE);
                }
            }
        }
    }
}

fn perform(ctx: &mut Ctx, fx: Vec<Effect>) {
    unsafe {
        for e in fx {
            match e {
                Effect::Open(_) => {
                    if let Some(d) = ctx.engine.show_deadline() {
                        let ms = d.saturating_duration_since(Instant::now()).as_millis() as u32;
                        SetTimer(ctx.overlay, TIMER_SHOW, ms.max(1), None);
                    }
                }
                Effect::Show => show_overlay(ctx),
                Effect::Select(_) => {
                    if ctx.shown {
                        InvalidateRect(ctx.overlay, std::ptr::null(), 0);
                    }
                }
                Effect::Commit(i) => {
                    hide_overlay(ctx);
                    if let Some(w) = ctx.engine.item(i).cloned() {
                        ctx.mru.touch(w.id);
                        focus(&w);
                    }
                }
                Effect::Cancel => {
                    hide_overlay(ctx);
                    inject_dummy();
                }
                Effect::Act(a, i) => {
                    if let Some(w) = ctx.engine.item(i).cloned() {
                        act(a, &w);
                        // Re-list shortly after the app reacted.
                        SetTimer(ctx.overlay, 2, 250, None);
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------ overlay

unsafe fn clear_thumbs(ctx: &mut Ctx) {
    for t in ctx.thumbs.drain(..) {
        DwmUnregisterThumbnail(t);
    }
}

fn px(v: f64, s: f64) -> i32 {
    (v * s).round() as i32
}

unsafe fn show_overlay(ctx: &mut Ctx) {
    clear_thumbs(ctx);
    let s = ctx.scale;
    let p = ctx.engine.layout().panel;
    SetWindowPos(ctx.overlay, HWND_TOPMOST, px(p.x, s), px(p.y, s), px(p.w, s), px(p.h, s), SWP_NOACTIVATE);
    let rgn = CreateRoundRectRgn(0, 0, px(p.w, s) + 1, px(p.h, s) + 1, px(20.0, s), px(20.0, s));
    SetWindowRgn(ctx.overlay, rgn, 1);
    if !ctx.engine.icon_mode() {
        for (w, t) in ctx.engine.items().iter().zip(ctx.engine.layout().tiles.iter()) {
            let mut id: isize = 0;
            if DwmRegisterThumbnail(ctx.overlay, w.id as usize as HWND, &mut id) != 0 {
                ctx.thumbs.push(0);
                continue;
            }
            let mut size: SIZE = std::mem::zeroed();
            DwmQueryThumbnailSourceSize(id, &mut size);
            let area = t.thumb;
            let (sw, sh) = (size.cx.max(1) as f64, size.cy.max(1) as f64);
            let k = (area.w / sw).min(area.h / sh);
            let (fw, fh) = (sw * k, sh * k);
            let r = RectF::new(area.x + (area.w - fw) / 2.0, area.y + (area.h - fh) / 2.0, fw, fh);
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: RECT { left: px(r.x, s), top: px(r.y, s), right: px(r.right(), s), bottom: px(r.bottom(), s) },
                rcSource: std::mem::zeroed(),
                opacity: if w.minimized { 160 } else { 255 },
                fVisible: 1,
                fSourceClientAreaOnly: 0,
            };
            DwmUpdateThumbnailProperties(id, &props);
            ctx.thumbs.push(id);
        }
    }
    ctx.shown = true;
    SHOWN.store(true, std::sync::atomic::Ordering::SeqCst);
    ShowWindow(ctx.overlay, SW_SHOWNOACTIVATE);
    InvalidateRect(ctx.overlay, std::ptr::null(), 1);
}

unsafe fn hide_overlay(ctx: &mut Ctx) {
    KillTimer(ctx.overlay, TIMER_SHOW);
    if ctx.shown {
        ShowWindow(ctx.overlay, SW_HIDE);
        ctx.shown = false;
    }
    SHOWN.store(false, std::sync::atomic::Ordering::SeqCst);
    clear_thumbs(ctx);
}

fn accent() -> COLORREF {
    unsafe {
        let mut c: u32 = 0;
        let mut opaque: BOOL = 0;
        if DwmGetColorizationColor(&mut c, &mut opaque) == 0 {
            let (r, g, b) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
            return r | (g << 8) | (b << 16);
        }
        0x00D7_7800 // BGR of #0078D7
    }
}

const fn rgb(r: u32, g: u32, b: u32) -> COLORREF {
    r | (g << 8) | (b << 16)
}

unsafe fn icon_of(hwnd: HWND) -> HICON {
    let mut res: usize = 0;
    SendMessageTimeoutW(hwnd, WM_GETICON, ICON_BIG as usize, 0, SMTO_ABORTIFHUNG, 50, &mut res);
    if res == 0 {
        res = GetClassLongPtrW(hwnd, GCLP_HICON);
    }
    if res == 0 {
        res = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) as usize;
    }
    res as HICON
}

unsafe fn paint(ctx: &Ctx, hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old_bmp = SelectObject(mem, bmp);
    let bg = CreateSolidBrush(rgb(30, 30, 34));
    FillRect(mem, &rc, bg);
    DeleteObject(bg);

    let s = ctx.scale;
    let layout = ctx.engine.layout();
    let sel = ctx.engine.selected();
    SetBkMode(mem, TRANSPARENT as i32);
    let old_font = SelectObject(mem, ctx.font);
    if ctx.engine.items().is_empty() {
        SetTextColor(mem, rgb(200, 200, 205));
        let text = wide("No other windows");
        let mut r = rc;
        DrawTextW(mem, text.as_ptr(), -1, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
    for (i, (item, t)) in ctx.engine.items().iter().zip(layout.tiles.iter()).enumerate() {
        if i == sel {
            let pen = CreatePen(PS_SOLID, px(2.5, s).max(2), accent());
            let br = CreateSolidBrush(rgb(58, 58, 64));
            let op = SelectObject(mem, pen);
            let ob = SelectObject(mem, br);
            let f = t.frame;
            RoundRect(mem, px(f.x, s), px(f.y, s), px(f.right(), s), px(f.bottom(), s), px(28.0, s), px(28.0, s));
            SelectObject(mem, op);
            SelectObject(mem, ob);
            DeleteObject(pen);
            DeleteObject(br);
        }
        let hwnd_src = item.id as usize as HWND;
        let icon = icon_of(hwnd_src);
        let has_thumb = !ctx.engine.icon_mode() && ctx.thumbs.get(i).is_some_and(|&t| t != 0);
        if !has_thumb {
            let a = t.thumb;
            let sz = a.w.min(a.h) * if ctx.engine.icon_mode() { 0.9 } else { 0.5 };
            DrawIconEx(mem, px(a.x + (a.w - sz) / 2.0, s), px(a.y + (a.h - sz) / 2.0, s), icon, px(sz, s), px(sz, s), 0, std::ptr::null_mut(), DI_NORMAL);
        }
        let l = t.label;
        let isz = 18.0;
        DrawIconEx(mem, px(l.x, s), px(l.y + (l.h - isz) / 2.0, s), icon, px(isz, s), px(isz, s), 0, std::ptr::null_mut(), DI_NORMAL);
        SetTextColor(mem, if i == sel { rgb(255, 255, 255) } else { rgb(215, 215, 220) });
        let text = wide(item.display_title());
        let mut r = RECT { left: px(l.x + isz + 6.0, s), top: px(l.y, s), right: px(l.right(), s), bottom: px(l.bottom(), s) };
        DrawTextW(mem, text.as_ptr(), -1, &mut r, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
    }
    SelectObject(mem, old_font);
    BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old_bmp);
    DeleteObject(bmp);
    DeleteDC(mem);
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let done = CTX.with(|c| {
                if let Ok(c) = c.try_borrow() {
                    if let Some(ctx) = c.as_ref() {
                        paint(ctx, hwnd);
                        return true;
                    }
                }
                false
            });
            if done {
                return 0;
            }
        }
        WM_TIMER => {
            KillTimer(hwnd, wp);
            with_ctx(|ctx| {
                if wp == TIMER_SHOW {
                    let fx = ctx.engine.tick(Instant::now());
                    perform(ctx, fx);
                } else {
                    refresh(ctx);
                }
            });
            return 0;
        }
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn with_ctx(f: impl FnOnce(&mut Ctx)) {
    CTX.with(|c| {
        if let Ok(mut c) = c.try_borrow_mut() {
            if let Some(ctx) = c.as_mut() {
                f(ctx);
            }
        }
    });
}

fn refresh(ctx: &mut Ctx) {
    let Some(mode) = ctx.engine.switcher().mode() else { return };
    let snap = snapshot(&ctx.mru, &ctx.engine.config().clone(), mode, ctx.scale);
    let (changed, fx) = ctx.engine.refresh(snap.items);
    if changed && ctx.shown {
        unsafe { show_overlay(ctx) };
    }
    perform(ctx, fx);
}

// ------------------------------------------------------------ hooks

fn mod_bit(vk: u32) -> Option<fn(&mut Mods) -> &mut bool> {
    Some(match vk {
        0x10 | 0xA0 | 0xA1 => |m| &mut m.shift,
        0x11 | 0xA2 | 0xA3 => |m| &mut m.ctrl,
        0x12 | 0xA4 | 0xA5 => |m| &mut m.alt,
        0x5B | 0x5C => |m| &mut m.win,
        _ => return None,
    })
}

unsafe extern "system" fn kb_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lp as *const KBDLLHOOKSTRUCT);
        if k.dwExtraInfo != pm_input::INJECTED_TAG {
            let down = wp as u32 == WM_KEYDOWN || wp as u32 == WM_SYSKEYDOWN;
            let mut swallow = false;
            with_ctx(|ctx| {
                if let Some(bit) = mod_bit(k.vkCode) {
                    *bit(&mut ctx.mods) = down;
                    if !down && ctx.engine.is_active() {
                        let fx = ctx.engine.modifiers_changed(ctx.mods);
                        perform(ctx, fx);
                    }
                    return;
                }
                if down {
                    let cfg = ctx.engine.config().clone();
                    let (mru, scale) = (ctx.mru.clone(), ctx.scale);
                    let o = ctx.engine.key_down(k.vkCode, ctx.mods, Instant::now(), &mut |m| snapshot(&mru, &cfg, m, scale));
                    swallow = o.swallow;
                    perform(ctx, o.effects);
                } else {
                    swallow = ctx.engine.key_up(k.vkCode).swallow;
                }
            });
            if swallow {
                return 1;
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wp, lp)
}

unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let m = &*(lp as *const MSLLHOOKSTRUCT);
        let mut swallow = false;
        with_ctx(|ctx| {
            if !ctx.shown {
                return;
            }
            let (x, y) = (m.pt.x as f64 / ctx.scale, m.pt.y as f64 / ctx.scale);
            match wp as u32 {
                WM_MOUSEMOVE => {
                    let fx = ctx.engine.hover(x, y);
                    perform(ctx, fx);
                }
                WM_LBUTTONDOWN => {
                    let o = ctx.engine.click(x, y);
                    swallow = o.swallow;
                    ctx.swallow_mouse_up = o.swallow;
                    perform(ctx, o.effects);
                }
                WM_LBUTTONUP => swallow = std::mem::take(&mut ctx.swallow_mouse_up),
                _ => {}
            }
        });
        if swallow {
            return 1;
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wp, lp)
}

unsafe extern "system" fn fg_hook(_h: HWINEVENTHOOK, _ev: u32, hwnd: HWND, id_object: i32, _c: i32, _t: u32, _ms: u32) {
    if id_object == OBJID_WINDOW && !hwnd.is_null() {
        with_ctx(|ctx| ctx.mru.touch(hwnd as usize as u64));
    }
}

// ------------------------------------------------------------ thread

fn run(cfg: Config, rx: Receiver<Cmd>, ready: Sender<Result<u32>>) {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("PowerMacWindowSwitcher");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&wc);
        let overlay = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if overlay.is_null() {
            let _ = ready.send(Err(anyhow!("CreateWindowExW failed")));
            return;
        }
        let pref: u32 = 2; // DWMWCP_ROUND
        DwmSetWindowAttribute(overlay, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &pref as *const u32 as *const _, 4);
        let scale = GetDpiForSystem() as f64 / 96.0;
        let face = wide("Segoe UI");
        let font = CreateFontW(-px(13.0, scale), 0, 0, 0, 500, 0, 0, 0, 0, 0, 0, CLEARTYPE_QUALITY as u32, 0, face.as_ptr());
        let mut mru = Mru::new();
        mru.seed(enumerate().into_iter().map(|w| w.id));
        CTX.with(|c| {
            *c.borrow_mut() = Some(Ctx {
                engine: Engine::new(cfg),
                mru,
                overlay,
                thumbs: vec![],
                mods: Mods::NONE,
                swallow_mouse_up: false,
                shown: false,
                scale,
                font,
            })
        });
        let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_hook), hinst, 0);
        let ms = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), hinst, 0);
        let fg = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, std::ptr::null_mut(), Some(fg_hook), 0, 0, WINEVENT_OUTOFCONTEXT);
        if kb.is_null() {
            let _ = ready.send(Err(anyhow!("SetWindowsHookExW(WH_KEYBOARD_LL) failed")));
            DestroyWindow(overlay);
            return;
        }
        let _ = ready.send(Ok(GetCurrentThreadId()));

        let mut msg: MSG = std::mem::zeroed();
        'outer: while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.message == WM_CMD {
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        Cmd::Stop => break 'outer,
                        Cmd::Update(cfg) => with_ctx(|ctx| {
                            let fx = ctx.engine.set_config(cfg);
                            perform(ctx, fx);
                        }),
                        Cmd::Show(mode) => with_ctx(|ctx| {
                            let cfg = ctx.engine.config().clone();
                            let (mru, scale) = (ctx.mru.clone(), ctx.scale);
                            let fx = ctx.engine.open_sticky(mode, Instant::now(), &mut |m| snapshot(&mru, &cfg, m, scale));
                            perform(ctx, fx);
                        }),
                        Cmd::Hide => with_ctx(|ctx| {
                            let fx = ctx.engine.cancel();
                            perform(ctx, fx);
                        }),
                    }
                }
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UnhookWindowsHookEx(kb);
        if !ms.is_null() {
            UnhookWindowsHookEx(ms);
        }
        if !fg.is_null() {
            UnhookWinEvent(fg);
        }
        with_ctx(|ctx| clear_thumbs(ctx));
        CTX.with(|c| {
            if let Some(ctx) = c.borrow_mut().take() {
                DeleteObject(ctx.font);
            }
        });
        DestroyWindow(overlay);
    }
}

pub(crate) struct Runtime {
    tx: Sender<Cmd>,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    config: Arc<Mutex<Config>>,
}

impl Runtime {
    pub(crate) fn start(cfg: Config) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (rtx, rrx) = mpsc::channel();
        let c = cfg.clone();
        let thread = std::thread::Builder::new().name("window-switcher".into()).spawn(move || run(c, rx, rtx))?;
        let thread_id = rrx.recv().map_err(|_| anyhow!("window switcher thread died"))??;
        Ok(Self { tx, thread_id, thread: Some(thread), config: Arc::new(Mutex::new(cfg)) })
    }

    fn post(&self, c: Cmd) {
        let _ = self.tx.send(c);
        unsafe { PostThreadMessageW(self.thread_id, WM_CMD, 0, 0) };
    }

    pub(crate) fn update(&mut self, cfg: Config) {
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = cfg.clone();
        self.post(Cmd::Update(cfg));
    }

    pub(crate) fn config(&self) -> Config {
        self.config.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn show(&self, mode: Mode) {
        self.post(Cmd::Show(mode));
    }

    pub(crate) fn hide(&self) {
        self.post(Cmd::Hide);
    }

    pub(crate) fn is_shown(&self) -> bool {
        SHOWN.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn debug_break_input(&self, _broken: bool) {}

    pub(crate) fn is_input_active(&self) -> bool {
        self.thread.is_some()
    }

    pub(crate) fn windows(&self) -> Vec<WindowInfo> {
        unsafe { enumerate() }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.post(Cmd::Stop);
            let _ = t.join();
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn permissions() -> crate::Permissions {
    crate::Permissions { accessibility: true, screen_recording: true }
}

