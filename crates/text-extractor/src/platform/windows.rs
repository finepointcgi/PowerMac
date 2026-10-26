//! Windows: `RegisterHotKey` thread; the screen is frozen (BitBlt of the
//! virtual desktop) into a dimmed full-desktop overlay where the selection
//! shows the original pixels; the selected pixels are read with
//! `Windows.Media.Ocr`. A small topmost window confirms the copy.

use std::cell::RefCell;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{anyhow, bail, Context, Result};
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapDecoder, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Storage::{FileAccessMode, StorageFile};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect,
    FrameRect, GetDC, GetTextExtentPoint32W, InvalidateRect, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, SetWindowRgn, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
    DT_CENTER, DT_SINGLELINE, DT_VCENTER, HBITMAP, HDC, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetCapture, UnregisterHotKey, MOD_ALT, MOD_CONTROL,
    MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_ESCAPE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, LoadCursorW, PostQuitMessage, PostThreadMessageW, RegisterClassW, SetCursor,
    SetForegroundWindow, SetLayeredWindowAttributes, SetTimer, ShowWindow, SystemParametersInfoW,
    TranslateMessage, IDC_CROSS, LWA_ALPHA, MSG, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SPI_GETWORKAREA, SW_SHOW, SW_SHOWNOACTIVATE, WM_DESTROY,
    WM_ERASEBKGND, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT,
    WM_QUIT, WM_RBUTTONDOWN, WM_SETCURSOR, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::CaptureCallback;
use crate::text::Fragment;
use crate::{Config, Hotkey};

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
            .name("text-extractor-hotkey".into())
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

// ---------------------------------------------------------------- OCR

fn init_apartment() {
    // Fails harmlessly if the thread already has an apartment.
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

fn engine(cfg: &Config) -> Result<OcrEngine> {
    for tag in cfg
        .languages
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
    {
        let lang = Language::CreateLanguage(&HSTRING::from(tag))?;
        if OcrEngine::IsLanguageSupported(&lang)? {
            return OcrEngine::TryCreateFromLanguage(&lang).context("creating the OCR engine");
        }
        log::warn!("text-extractor: OCR language {tag} is not installed");
    }
    OcrEngine::TryCreateFromUserProfileLanguages()
        .context("no OCR language is installed (Settings → Time & language → Language)")
}

fn recognize(bmp: &SoftwareBitmap, cfg: &Config) -> Result<Vec<Fragment>> {
    let engine = engine(cfg)?;
    let (w, h) = (bmp.PixelWidth()? as f64, bmp.PixelHeight()? as f64);
    let result = engine.RecognizeAsync(bmp)?.join()?;
    let mut out = Vec::new();
    for line in result.Lines()? {
        for word in line.Words()? {
            let r = word.BoundingRect()?;
            out.push(Fragment::new(
                word.Text()?.to_string(),
                r.X as f64 / w,
                r.Y as f64 / h,
                r.Width as f64 / w,
                r.Height as f64 / h,
            ));
        }
    }
    Ok(out)
}

/// Upscale small images (nearest neighbour) so the recognizer has enough
/// pixels per glyph, as PowerToys does.
fn upscale_bgra(w: usize, h: usize, px: Vec<u8>) -> (usize, usize, Vec<u8>) {
    let k = (120usize.div_ceil(w.min(h).max(1))).clamp(1, 4);
    if k == 1 {
        return (w, h, px);
    }
    let (nw, nh) = (w * k, h * k);
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        for x in 0..nw {
            let s = ((y / k) * w + x / k) * 4;
            let d = (y * nw + x) * 4;
            out[d..d + 4].copy_from_slice(&px[s..s + 4]);
        }
    }
    (nw, nh, out)
}

fn ocr_bgra(w: usize, h: usize, bgra: Vec<u8>, cfg: &Config) -> Result<Vec<Fragment>> {
    init_apartment();
    let (w, h, bgra) = upscale_bgra(w, h, bgra);
    let writer = DataWriter::new()?;
    writer.WriteBytes(&bgra)?;
    let buffer = writer.DetachBuffer()?;
    let bmp = SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        w as i32,
        h as i32,
    )?;
    recognize(&bmp, cfg)
}

pub fn ocr_rgba(w: usize, h: usize, rgba: &[u8], cfg: &Config) -> Result<Vec<Fragment>> {
    let mut bgra = Vec::with_capacity(w * h * 4);
    for p in rgba.as_chunks::<4>().0.iter().take(w * h) {
        let a = p[3] as u32;
        let over = |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        bgra.extend_from_slice(&[over(p[2]), over(p[1]), over(p[0]), 255]);
    }
    ocr_bgra(w, h, bgra, cfg)
}

pub fn ocr_file(path: &Path, cfg: &Config) -> Result<Vec<Fragment>> {
    init_apartment();
    let abs = std::path::absolute(path)?;
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(abs.to_string_lossy().as_ref()))?
        .join()?;
    let stream = file.OpenAsync(FileAccessMode::Read)?.join()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?
        .join()
        .context("not an image")?;
    let bmp = decoder.GetSoftwareBitmapAsync()?.join()?;
    let bmp = SoftwareBitmap::Convert(&bmp, BitmapPixelFormat::Bgra8)?;
    recognize(&bmp, cfg)
}

pub fn supported_languages() -> Vec<String> {
    init_apartment();
    OcrEngine::AvailableRecognizerLanguages()
        .map(|v| {
            v.into_iter()
                .filter_map(|l| l.LanguageTag().ok().map(|t| t.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn screen_recording_granted() -> bool {
    true
}

pub fn request_screen_recording() -> bool {
    true
}

// ---------------------------------------------------------------- selection overlay

struct Frozen {
    /// Virtual desktop origin and size (physical pixels).
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    /// Memory DCs holding the original and the dimmed screen.
    dc: HDC,
    dim_dc: HDC,
    bitmaps: [HBITMAP; 2],
    /// Top-down BGRA pixels of the original.
    bits: *const u8,
}

impl Drop for Frozen {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.dc);
            DeleteDC(self.dim_dc);
            for b in self.bitmaps {
                DeleteObject(b);
            }
        }
    }
}

struct Selection {
    frozen: Frozen,
    start: Option<(i32, i32)>,
    current: (i32, i32),
    result: Option<Option<RECT>>,
}

thread_local! {
    static SEL: RefCell<Option<Selection>> = const { RefCell::new(None) };
}

unsafe fn dib(screen: HDC, w: i32, h: i32) -> (HBITMAP, *mut u8) {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        },
        ..std::mem::zeroed()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let bmp = CreateDIBSection(
        screen,
        &bmi,
        DIB_RGB_COLORS,
        &mut bits,
        std::ptr::null_mut(),
        0,
    );
    (bmp, bits as *mut u8)
}

/// Copy the whole virtual desktop, plus a dimmed copy.
unsafe fn freeze() -> Option<Frozen> {
    let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    if w <= 0 || h <= 0 {
        return None;
    }
    let screen = GetDC(std::ptr::null_mut());
    let (bmp, bits) = dib(screen, w, h);
    let (dim_bmp, dim_bits) = dib(screen, w, h);
    if bmp.is_null() || dim_bmp.is_null() || bits.is_null() || dim_bits.is_null() {
        ReleaseDC(std::ptr::null_mut(), screen);
        return None;
    }
    let dc = CreateCompatibleDC(screen);
    let dim_dc = CreateCompatibleDC(screen);
    SelectObject(dc, bmp);
    SelectObject(dim_dc, dim_bmp);
    BitBlt(dc, 0, 0, w, h, screen, x, y, SRCCOPY | CAPTUREBLT);
    ReleaseDC(std::ptr::null_mut(), screen);
    let n = (w as usize) * (h as usize) * 4;
    let src = std::slice::from_raw_parts(bits, n);
    let dst = std::slice::from_raw_parts_mut(dim_bits, n);
    for (d, s) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
    {
        d[0] = (s[0] as u32 * 150 / 255) as u8;
        d[1] = (s[1] as u32 * 150 / 255) as u8;
        d[2] = (s[2] as u32 * 150 / 255) as u8;
        d[3] = 255;
    }
    Some(Frozen {
        x,
        y,
        w,
        h,
        dc,
        dim_dc,
        bitmaps: [bmp, dim_bmp],
        bits,
    })
}

fn norm_rect(a: (i32, i32), b: (i32, i32)) -> RECT {
    RECT {
        left: a.0.min(b.0),
        top: a.1.min(b.1),
        right: a.0.max(b.0),
        bottom: a.1.max(b.1),
    }
}

fn point(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam & 0xFFFF) as i16 as i32,
        ((lparam >> 16) & 0xFFFF) as i16 as i32,
    )
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    SEL.with(|s| {
        let s = s.borrow();
        let Some(s) = s.as_ref() else { return };
        let f = &s.frozen;
        // Back buffer to avoid flicker.
        let back = CreateCompatibleDC(dc);
        let bmp = CreateCompatibleBitmap(dc, f.w, f.h);
        let old = SelectObject(back, bmp);
        BitBlt(back, 0, 0, f.w, f.h, f.dim_dc, 0, 0, SRCCOPY);
        if let Some(a) = s.start {
            let r = norm_rect(a, s.current);
            BitBlt(
                back,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                f.dc,
                r.left,
                r.top,
                SRCCOPY,
            );
            let white = CreateSolidBrush(0x00FFFFFF);
            let frame = RECT {
                left: r.left - 1,
                top: r.top - 1,
                right: r.right + 1,
                bottom: r.bottom + 1,
            };
            FrameRect(back, &frame, white);
            DeleteObject(white);
        }
        BitBlt(dc, 0, 0, f.w, f.h, back, 0, 0, SRCCOPY);
        SelectObject(back, old);
        DeleteObject(bmp);
        DeleteDC(back);
    });
    EndPaint(hwnd, &ps);
}

fn finish_selection(hwnd: HWND, r: Option<RECT>) {
    SEL.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.result = Some(r);
        }
    });
    unsafe { DestroyWindow(hwnd) };
}

unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_SETCURSOR => {
            SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_CROSS));
            1
        }
        WM_LBUTTONDOWN => {
            let p = point(lparam);
            SEL.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.start = Some(p);
                    s.current = p;
                }
            });
            SetCapture(hwnd);
            InvalidateRect(hwnd, std::ptr::null(), 0);
            0
        }
        WM_MOUSEMOVE => {
            let p = point(lparam);
            let dragging = SEL.with(|s| {
                s.borrow_mut().as_mut().map(|s| {
                    s.current = p;
                    s.start.is_some()
                })
            });
            if dragging == Some(true) {
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            let p = point(lparam);
            let r = SEL.with(|s| {
                let mut b = s.borrow_mut();
                let s = b.as_mut()?;
                let a = s.start.take()?;
                Some(norm_rect(a, p))
            });
            match r {
                Some(r) if r.right - r.left >= 4 && r.bottom - r.top >= 4 => {
                    finish_selection(hwnd, Some(r))
                }
                _ => {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            0
        }
        WM_RBUTTONDOWN => {
            finish_selection(hwnd, None);
            0
        }
        WM_KEYDOWN if wparam as u16 == VK_ESCAPE => {
            finish_selection(hwnd, None);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Run the overlay on this thread; returns the selected BGRA pixels.
fn select_pixels() -> Result<Option<(usize, usize, Vec<u8>)>> {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let frozen = freeze().ok_or_else(|| anyhow!("could not capture the screen"))?;
        let (x, y, w, h) = (frozen.x, frozen.y, frozen.w, frozen.h);
        SEL.with(|s| {
            *s.borrow_mut() = Some(Selection {
                frozen,
                start: None,
                current: (0, 0),
                result: None,
            })
        });
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("PowerMacTextExtractorOverlay");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(overlay_proc),
            hInstance: hinst,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            x,
            y,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            SEL.with(|s| s.borrow_mut().take());
            bail!("could not create the selection window");
        }
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let sel = SEL
            .with(|s| s.borrow_mut().take())
            .ok_or_else(|| anyhow!("selection state lost"))?;
        let Some(Some(r)) = sel.result else {
            return Ok(None);
        };
        let f = &sel.frozen;
        let (l, t) = (r.left.clamp(0, f.w), r.top.clamp(0, f.h));
        let (rr, b) = (r.right.clamp(0, f.w), r.bottom.clamp(0, f.h));
        let (cw, ch) = ((rr - l) as usize, (b - t) as usize);
        if cw == 0 || ch == 0 {
            return Ok(None);
        }
        let src = std::slice::from_raw_parts(f.bits, (f.w as usize) * (f.h as usize) * 4);
        let mut out = Vec::with_capacity(cw * ch * 4);
        for row in t as usize..b as usize {
            let start = (row * f.w as usize + l as usize) * 4;
            out.extend_from_slice(&src[start..start + cw * 4]);
        }
        for p in out.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        Ok(Some((cw, ch, out)))
    }
}

pub fn capture_selection(cfg: Config, cb: CaptureCallback) {
    std::thread::spawn(move || {
        let res = select_pixels().and_then(|sel| match sel {
            None => Ok(None),
            Some((w, h, px)) => {
                let frags = ocr_bgra(w, h, px, &cfg)?;
                Ok(Some(crate::text::assemble(frags, cfg.single_line)))
            }
        });
        cb(res);
    });
}

pub fn capture_blocking(cfg: Config) -> Result<Option<String>> {
    let (tx, rx) = mpsc::channel();
    capture_selection(
        cfg,
        Box::new(move |r| {
            let _ = tx.send(r);
        }),
    );
    rx.recv().unwrap_or(Ok(None))
}

pub fn cancel_selection() {}

// ---------------------------------------------------------------- toast

thread_local! {
    static TOAST_TEXT: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "system" fn toast_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
            let bg = CreateSolidBrush(0x00211D1C);
            FillRect(dc, &rc, bg);
            DeleteObject(bg);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, 0x00FFFFFF);
            let font = CreateFontW(
                -16,
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
            TOAST_TEXT.with(|t| {
                let mut t = t.borrow().clone();
                DrawTextW(
                    dc,
                    t.as_mut_ptr(),
                    t.len() as i32,
                    &mut rc,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
            });
            SelectObject(dc, old);
            DeleteObject(font);
            EndPaint(hwnd, &ps);
            0
        }
        WM_TIMER => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub fn toast(text: &str) {
    let text = text.to_string();
    std::thread::spawn(move || unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let t: Vec<u16> = text.encode_utf16().collect();
        TOAST_TEXT.with(|c| *c.borrow_mut() = t.clone());
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("PowerMacTextExtractorToast");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(toast_proc),
            hInstance: hinst,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        // Measure the text.
        let screen = GetDC(std::ptr::null_mut());
        let font = CreateFontW(
            -16,
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
        let old = SelectObject(screen, font);
        let mut size: SIZE = std::mem::zeroed();
        GetTextExtentPoint32W(screen, t.as_ptr(), t.len() as i32, &mut size);
        SelectObject(screen, old);
        DeleteObject(font);
        ReleaseDC(std::ptr::null_mut(), screen);
        let (w, h) = (size.cx + 48, size.cy + 22);
        let mut work: RECT = std::mem::zeroed();
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut work as *mut RECT as *mut std::ffi::c_void,
            0,
        );
        let x = work.left + (work.right - work.left - w) / 2;
        let y = work.bottom - h - 80;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            class.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            x,
            y,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }
        SetLayeredWindowAttributes(hwnd, 0, 235, LWA_ALPHA);
        SetWindowRgn(hwnd, CreateRoundRectRgn(0, 0, w + 1, h + 1, h, h), 1);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, 1, 1800, None);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}
