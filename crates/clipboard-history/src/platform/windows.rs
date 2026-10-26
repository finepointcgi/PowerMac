//! Windows: a message-only window that gets `WM_CLIPBOARDUPDATE`
//! (`AddClipboardFormatListener`) and `WM_HOTKEY` (`RegisterHotKey`), plus
//! clipboard reading/writing with the Win32 clipboard API.
//!
//! Compile-checked only; not run on Windows yet.

#![allow(clippy::upper_case_acronyms)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, EnumClipboardFormats,
    GetClipboardData, GetClipboardFormatNameW, GetClipboardOwner, GetClipboardSequenceNumber,
    OpenClipboard, RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};
use windows_sys::Win32::UI::Shell::{DragQueryFileW, DROPFILES, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, PostMessageW, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, TranslateMessage, HWND_MESSAGE, MSG, WM_CLIPBOARDUPDATE,
    WM_CLOSE, WM_DESTROY, WM_HOTKEY, WNDCLASSW,
};

use crate::content::{to_png, Content, SourceApp};

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn register(name: &str) -> u32 {
    unsafe { RegisterClipboardFormatW(wide(name).as_ptr()) }
}

pub fn change_count() -> i64 {
    unsafe { GetClipboardSequenceNumber() as i64 }
}

/// Opens the clipboard (retrying while another app holds it) and closes it
/// on drop.
struct Open;

impl Open {
    fn new() -> Result<Open> {
        for _ in 0..20 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Ok(Open);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        bail!("the clipboard is busy")
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

unsafe fn global_bytes(h: *mut c_void) -> Option<Vec<u8>> {
    if h.is_null() {
        return None;
    }
    let p = GlobalLock(h) as *const u8;
    if p.is_null() {
        return None;
    }
    let n = GlobalSize(h);
    let v = std::slice::from_raw_parts(p, n).to_vec();
    GlobalUnlock(h);
    Some(v)
}

fn format_name(f: u32) -> String {
    match f {
        1 => "CF_TEXT".into(),
        2 => "CF_BITMAP".into(),
        7 => "CF_OEMTEXT".into(),
        CF_DIB => "CF_DIB".into(),
        CF_UNICODETEXT => "CF_UNICODETEXT".into(),
        CF_HDROP => "CF_HDROP".into(),
        16 => "CF_LOCALE".into(),
        CF_DIBV5 => "CF_DIBV5".into(),
        _ => {
            let mut buf = [0u16; 256];
            let n = unsafe { GetClipboardFormatNameW(f, buf.as_mut_ptr(), buf.len() as i32) };
            if n > 0 {
                String::from_utf16_lossy(&buf[..n as usize])
            } else {
                format!("#{f}")
            }
        }
    }
}

fn exe_name(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(h);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Some(path.rsplit('\\').next().unwrap_or(&path).to_string())
    }
}

fn window_app(hwnd: HWND) -> Option<(u32, SourceApp)> {
    if hwnd.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    let exe = exe_name(pid)?;
    let name = exe.strip_suffix(".exe").unwrap_or(&exe).to_string();
    Some((
        pid,
        SourceApp {
            name: Some(name),
            id: Some(exe),
        },
    ))
}

/// Format names (with `CanIncludeInClipboardHistory=0` when it says no) and
/// the owner app (falling back to the foreground app).
pub fn read_meta() -> (Vec<String>, Option<SourceApp>) {
    let Ok(_open) = Open::new() else {
        return (Vec::new(), None);
    };
    let can_include = register("CanIncludeInClipboardHistory");
    let mut types = Vec::new();
    let mut f = 0u32;
    loop {
        f = unsafe { EnumClipboardFormats(f) };
        if f == 0 {
            break;
        }
        types.push(format_name(f));
        if f == can_include {
            let v = unsafe { global_bytes(GetClipboardData(f)) };
            if v.as_deref()
                .is_some_and(|b| b.len() >= 4 && b[..4] == [0, 0, 0, 0])
            {
                types.push("CanIncludeInClipboardHistory=0".into());
            }
        }
    }
    let owner = unsafe { GetClipboardOwner() };
    let source = window_app(owner)
        .or_else(|| window_app(unsafe { GetForegroundWindow() }))
        .map(|(_, a)| a);
    (types, source)
}

pub fn read_content(_types: &[String]) -> Content {
    let Ok(_open) = Open::new() else {
        return Content::default();
    };
    let mut c = Content::default();
    unsafe {
        if let Some(b) = global_bytes(GetClipboardData(CF_UNICODETEXT)) {
            let w: Vec<u16> = b
                .as_chunks::<2>()
                .0
                .iter()
                .map(|x| u16::from_le_bytes(*x))
                .take_while(|&u| u != 0)
                .collect();
            c.text = Some(String::from_utf16_lossy(&w));
        }
        if let Some(b) = global_bytes(GetClipboardData(register("HTML Format"))) {
            let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
            c.html = Some(String::from_utf8_lossy(&b[..end]).into_owned());
        }
        if let Some(b) = global_bytes(GetClipboardData(register("Rich Text Format"))) {
            let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
            c.rtf = Some(b[..end].to_vec());
        }
        let hdrop = GetClipboardData(CF_HDROP) as HDROP;
        if !hdrop.is_null() {
            let n = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
            for i in 0..n {
                let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
                let mut buf = vec![0u16; len as usize + 1];
                DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
                c.files.push(String::from_utf16_lossy(&buf[..len as usize]));
            }
        }
        if c.files.is_empty() {
            if let Some(png) = global_bytes(GetClipboardData(register("PNG"))) {
                c.png = to_png(&png).map(|(p, _, _)| p);
            }
            if c.png.is_none() {
                let dib = global_bytes(GetClipboardData(CF_DIBV5))
                    .or_else(|| global_bytes(GetClipboardData(CF_DIB)));
                if let Some(d) = dib {
                    c.png = dib_to_png(&d);
                }
            }
        }
    }
    c
}

/// A DIB (BITMAPINFOHEADER + pixels) to PNG, by prefixing a file header and
/// letting the BMP decoder do the work.
fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let header_size = u32::from_le_bytes(dib[0..4].try_into().ok()?);
    let bit_count = u16::from_le_bytes(dib[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
    let clr_used = u32::from_le_bytes(dib[32..36].try_into().ok()?);
    let palette = if bit_count <= 8 {
        (if clr_used == 0 {
            1u32 << bit_count
        } else {
            clr_used
        }) * 4
    } else {
        0
    };
    let masks = if compression == 3 && header_size == 40 {
        12
    } else {
        0
    };
    let offset = 14 + header_size + palette + masks;
    let mut bmp = Vec::with_capacity(dib.len() + 14);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&((dib.len() + 14) as u32).to_le_bytes());
    bmp.extend_from_slice(&[0, 0, 0, 0]);
    bmp.extend_from_slice(&offset.to_le_bytes());
    bmp.extend_from_slice(dib);
    to_png(&bmp).map(|(p, _, _)| p)
}

/// PNG to a CF_DIB (BMP without its file header).
fn png_to_dib(png: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img.to_rgb8())
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Bmp)
        .ok()?;
    (out.len() > 14).then(|| out[14..].to_vec())
}

unsafe fn set_bytes(format: u32, bytes: &[u8]) -> bool {
    let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
    if h.is_null() {
        return false;
    }
    let p = GlobalLock(h) as *mut u8;
    if p.is_null() {
        return false;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    GlobalUnlock(h);
    !SetClipboardData(format, h).is_null()
}

pub fn write(c: &Content, plain: bool) -> Result<i64> {
    let _open = Open::new()?;
    unsafe {
        EmptyClipboard();
        let mut any = false;
        let text = c
            .text
            .clone()
            .or_else(|| (plain && !c.files.is_empty()).then(|| c.files.join("\r\n")));
        if let Some(t) = &text {
            let w: Vec<u8> = t
                .encode_utf16()
                .chain(std::iter::once(0))
                .flat_map(|u| u.to_le_bytes())
                .collect();
            any |= set_bytes(CF_UNICODETEXT, &w);
        }
        if !plain {
            if let Some(h) = &c.html {
                let mut b = h.as_bytes().to_vec();
                b.push(0);
                any |= set_bytes(register("HTML Format"), &b);
            }
            if let Some(r) = &c.rtf {
                let mut b = r.clone();
                b.push(0);
                any |= set_bytes(register("Rich Text Format"), &b);
            }
            if let Some(p) = &c.png {
                any |= set_bytes(register("PNG"), p);
                if let Some(d) = png_to_dib(p) {
                    any |= set_bytes(CF_DIB, &d);
                }
            }
            if !c.files.is_empty() {
                let mut list: Vec<u16> = Vec::new();
                for f in &c.files {
                    list.extend(f.encode_utf16());
                    list.push(0);
                }
                list.push(0);
                let header = std::mem::size_of::<DROPFILES>();
                let mut b = vec![0u8; header];
                let df = DROPFILES {
                    pFiles: header as u32,
                    pt: POINT { x: 0, y: 0 },
                    fNC: 0,
                    fWide: 1,
                };
                std::ptr::copy_nonoverlapping(
                    &df as *const DROPFILES as *const u8,
                    b.as_mut_ptr(),
                    header,
                );
                b.extend(list.iter().flat_map(|u| u.to_le_bytes()));
                any |= set_bytes(CF_HDROP, &b);
            }
        }
        if !any {
            bail!("nothing to put on the clipboard");
        }
    }
    drop(_open);
    Ok(change_count())
}

// ---------------------------------------------------------------------------
// Apps / pointer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FrontApp {
    pub pid: i32,
    /// The foreground window (HWND as an integer).
    pub window: isize,
    pub app: SourceApp,
}

pub fn frontmost() -> Option<FrontApp> {
    let hwnd = unsafe { GetForegroundWindow() };
    let (pid, app) = window_app(hwnd)?;
    Some(FrontApp {
        pid: pid as i32,
        window: hwnd as isize,
        app,
    })
}

pub fn activate(_pid: i32, window: isize) -> bool {
    window != 0 && unsafe { SetForegroundWindow(window as HWND) } != 0
}

pub fn is_frontmost(pid: i32) -> bool {
    frontmost().is_some_and(|f| f.pid == pid)
}

/// Pointer position and its monitor's work area, in logical pixels.
pub fn pointer_and_screen() -> Option<([f32; 2], [f32; 4])> {
    unsafe {
        let mut p = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut p) == 0 {
            return None;
        }
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi) == 0 {
            return None;
        }
        let (mut dx, mut dy) = (96u32, 96u32);
        GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let s = dx.max(1) as f32 / 96.0;
        let r = mi.rcWork;
        Some((
            [p.x as f32 / s, p.y as f32 / s],
            [
                r.left as f32 / s,
                r.top as f32 / s,
                (r.right - r.left) as f32 / s,
                (r.bottom - r.top) as f32 / s,
            ],
        ))
    }
}

// ---------------------------------------------------------------------------
// Listener window
// ---------------------------------------------------------------------------

pub enum Event {
    Changed,
    Hotkey,
}

thread_local! {
    static SINK: RefCell<Option<mpsc::Sender<Event>>> = const { RefCell::new(None) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => {
            SINK.with(|s| {
                if let Some(tx) = s.borrow().as_ref() {
                    let _ = tx.send(Event::Changed);
                }
            });
            0
        }
        WM_HOTKEY => {
            SINK.with(|s| {
                if let Some(tx) = s.borrow().as_ref() {
                    let _ = tx.send(Event::Hotkey);
                }
            });
            0
        }
        WM_CLOSE => {
            RemoveClipboardFormatListener(hwnd);
            UnregisterHotKey(hwnd, 1);
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

struct SendHwnd(isize);

/// The listener window thread; dropping it stops listening.
pub struct Listener {
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
    pub hotkey_ok: bool,
}

impl Listener {
    pub fn start(
        hotkey: Option<keyboard_manager::Shortcut>,
        tx: mpsc::Sender<Event>,
    ) -> Result<Listener> {
        let (rtx, rrx) = mpsc::channel::<std::result::Result<(SendHwnd, bool), String>>();
        let thread = std::thread::Builder::new()
            .name("cliphist-win".into())
            .spawn(move || unsafe {
                SINK.with(|s| *s.borrow_mut() = Some(tx));
                let class = wide("PowerMacClipboardHistory");
                let hinst = GetModuleHandleW(std::ptr::null());
                let mut wc: WNDCLASSW = std::mem::zeroed();
                wc.lpfnWndProc = Some(wndproc);
                wc.hInstance = hinst;
                wc.lpszClassName = class.as_ptr();
                RegisterClassW(&wc);
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
                    let _ = rtx.send(Err("could not create the clipboard listener window".into()));
                    return;
                }
                if AddClipboardFormatListener(hwnd) == 0 {
                    DestroyWindow(hwnd);
                    let _ = rtx.send(Err("AddClipboardFormatListener failed".into()));
                    return;
                }
                let mut hotkey_ok = false;
                if let Some(h) = hotkey {
                    let mut m = MOD_NOREPEAT;
                    use keyboard_manager::keys::Side;
                    if h.mods.win != Side::None {
                        m |= MOD_WIN;
                    }
                    if h.mods.ctrl != Side::None {
                        m |= MOD_CONTROL;
                    }
                    if h.mods.alt != Side::None {
                        m |= MOD_ALT;
                    }
                    if h.mods.shift != Side::None {
                        m |= MOD_SHIFT;
                    }
                    hotkey_ok = RegisterHotKey(hwnd, 1, m, h.key) != 0;
                    if !hotkey_ok {
                        log::warn!("clipboard history: hotkey {h} is taken by another app");
                    }
                }
                let _ = rtx.send(Ok((SendHwnd(hwnd as isize), hotkey_ok)));
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                SINK.with(|s| *s.borrow_mut() = None);
            })?;
        match rrx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok((h, hotkey_ok))) => Ok(Listener {
                hwnd: h.0,
                thread: Some(thread),
                hotkey_ok,
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(anyhow!(e))
            }
            Err(_) => Err(anyhow!("clipboard listener did not start")),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Not used on Windows (the listener window reports changes).
pub const POLL: Duration = Duration::from_millis(500);

/// Hotkeys are registered by the [`Listener`] on Windows.
pub struct Hotkey;

impl Hotkey {
    pub fn start(
        _hotkey: keyboard_manager::Shortcut,
        _on_fire: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Hotkey> {
        bail!("on Windows the hotkey is registered by the clipboard listener")
    }
}
