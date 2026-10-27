//! Windows backend (PowerToys' approach; compile-checked, not yet run on
//! Windows): `EnumWindows` with PowerToys' filters (visible, titled,
//! resizable, not a tool / owned window, not cloaked except by another
//! virtual desktop), process image paths, `GetWindowPlacement` for the
//! state, `SetWindowPlacement` with work-area coordinates for placing, and
//! `ShellExecuteExW` (verb `runas` for elevated entries) with the recorded
//! command line arguments for launching. Monitor ids are FancyZones'
//! (device interface path).

#![allow(clippy::missing_safety_doc)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWM_CLOAKED_SHELL};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NO_CONSOLE, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::backend::{AppIdentity, Backend, LaunchRequest, LiveWindow, Placement};
use crate::capture::CaptureOptions;
use crate::geometry::{Monitor, Rect};
use crate::matching::InstalledApp;
use crate::model::WindowState;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(b: &[u16]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf16_lossy(&b[..end])
}

fn to_rect(r: &RECT) -> Rect {
    Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

fn to_win(r: &Rect) -> RECT {
    RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

/// Per-monitor DPI awareness for this thread while alive (physical pixels).
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

pub fn has_permissions() -> bool {
    true
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

/// Monitors with FancyZones-compatible ids, primary first.
pub fn monitors() -> Result<Vec<Monitor>> {
    let _dpi = DpiGuard::new();
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(enum_monitor), &mut handles as *mut _ as LPARAM);
    }
    let mut out: Vec<Monitor> = Vec::new();
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
            if out.iter().any(|m| m.id == id) {
                id = format!("{id}#{device}");
            }
            out.push(Monitor {
                id,
                name: device,
                number: 0,
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
    out.sort_by_key(|m| !m.primary);
    for (i, m) in out.iter_mut().enumerate() {
        m.number = i as u32;
    }
    Ok(out)
}

pub fn installed_apps() -> Vec<InstalledApp> {
    vec![]
}

pub fn app_icon_rgba(_path: &str, _bundle_id: &str, _size: u32) -> Option<Vec<u8>> {
    None
}

pub fn app_exists(path: &str, _bundle_id: &str) -> bool {
    Path::new(path).exists()
}

fn hidden_command(program: &str) -> std::process::Command {
    let mut c = std::process::Command::new(program);
    use std::os::windows::process::CommandExt;
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}

/// A file chooser (PowerShell's OpenFileDialog; blocking).
pub fn pick_file(prompt: &str) -> Option<PathBuf> {
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Title = {}; $d.Filter = 'JSON (*.json)|*.json|All files (*.*)|*.*'; if ($d.ShowDialog() -eq 'OK') {{ $d.FileName }}",
        crate::shortcut::ps_quote(prompt)
    );
    let out = hidden_command("powershell").args(["-NoProfile", "-STA", "-Command", &script]).output().ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!p.is_empty()).then(|| PathBuf::from(p))
}

pub fn reveal(path: &Path) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
}

pub fn become_foreground_app() {}

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

unsafe fn is_elevated(pid: u32) -> bool {
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if h.is_null() {
        return false;
    }
    let mut token: HANDLE = std::ptr::null_mut();
    let mut out = false;
    if OpenProcessToken(h, TOKEN_QUERY, &mut token) != 0 {
        let mut e: TOKEN_ELEVATION = std::mem::zeroed();
        let mut size = 0u32;
        if GetTokenInformation(token, TokenElevation, &mut e as *mut _ as *mut _, std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut size) != 0 {
            out = e.TokenIsElevated != 0;
        }
        CloseHandle(token);
    }
    CloseHandle(h);
    out
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

unsafe fn placement(hwnd: HWND) -> Option<WINDOWPLACEMENT> {
    let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
    wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
    (GetWindowPlacement(hwnd, &mut wp) != 0).then_some(wp)
}

/// Offset of the work area within its monitor (`rcNormalPosition` is in
/// work-area coordinates).
unsafe fn work_offset(r: &RECT) -> (i32, i32) {
    let m = MonitorFromRect(r, MONITOR_DEFAULTTOPRIMARY);
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(m, &mut mi) == 0 {
        return (0, 0);
    }
    (mi.rcWork.left - mi.rcMonitor.left, mi.rcWork.top - mi.rcMonitor.top)
}

struct EnumCtx {
    own: u32,
    out: Vec<LiveWindow>,
}

unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam as *mut EnumCtx);
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    let owner = GetWindow(hwnd, GW_OWNER);
    let app_window = ex & WS_EX_APPWINDOW != 0;
    if (ex & WS_EX_TOOLWINDOW != 0 && !app_window) || (!owner.is_null() && !app_window) || ex & WS_EX_NOACTIVATE != 0 {
        return 1;
    }
    // PowerToys: only resizable windows (thick frame), no popups.
    if style & WS_THICKFRAME == 0 || (style & WS_POPUP != 0 && style & WS_CAPTION == 0) {
        return 1;
    }
    let title = window_text(hwnd);
    if title.is_empty() {
        return 1;
    }
    let c = cloaked(hwnd);
    if c != 0 && c != DWM_CLOAKED_SHELL {
        return 1;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == ctx.own {
        return 1;
    }
    let path = exe_path(pid);
    if path.is_empty() {
        return 1;
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with("\\explorer.exe") && (title == "Program Manager") {
        return 1;
    }
    let name = path.rsplit('\\').next().unwrap_or("").trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
    let mut r: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut r);
    let minimized = IsIconic(hwnd) != 0;
    if minimized {
        if let Some(wp) = placement(hwnd) {
            let n = wp.rcNormalPosition;
            let (ox, oy) = work_offset(&n);
            r = RECT { left: n.left + ox, top: n.top + oy, right: n.right + ox, bottom: n.bottom + oy };
        }
    }
    let state = if minimized {
        WindowState::Minimized
    } else if IsZoomed(hwnd) != 0 {
        WindowState::Zoomed
    } else {
        WindowState::Normal
    };
    ctx.out.push(LiveWindow {
        id: hwnd as usize as u64,
        pid: pid as i64,
        app: AppIdentity { bundle_id: String::new(), path, name },
        title,
        frame: to_rect(&r),
        state,
        space: None,
        document: None,
        url: None,
        app_hidden: false,
        elevated: is_elevated(pid),
    });
    1
}

pub struct Native {
    start: Instant,
}

impl Native {
    pub fn new(_opts: &CaptureOptions) -> Self {
        Self { start: Instant::now() }
    }
}

fn hwnd_of(w: &LiveWindow) -> HWND {
    w.id as usize as HWND
}

impl Backend for Native {
    fn monitors(&self) -> Result<Vec<Monitor>> {
        monitors()
    }

    fn windows(&self) -> Result<Vec<LiveWindow>> {
        let _dpi = DpiGuard::new();
        let mut ctx = EnumCtx { own: unsafe { GetCurrentProcessId() }, out: Vec::new() };
        unsafe { EnumWindows(Some(enum_window), &mut ctx as *mut _ as LPARAM) };
        Ok(ctx.out)
    }

    fn launch(&self, req: &LaunchRequest) -> Result<()> {
        if !Path::new(&req.path).exists() {
            bail!("file not found: {}", req.path);
        }
        let mut params = req.args.clone();
        if let Some(o) = &req.open {
            if !params.is_empty() {
                params.push(' ');
            }
            params.push_str(&format!("\"{o}\""));
        }
        let dir = Path::new(&req.path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let verb = wide(if req.as_admin { "runas" } else { "open" });
        let file = wide(&req.path);
        let params_w = wide(&params);
        let dir_w = wide(&dir);
        unsafe {
            let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
            sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            sei.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE;
            sei.lpVerb = verb.as_ptr();
            sei.lpFile = file.as_ptr();
            sei.lpParameters = params_w.as_ptr();
            sei.lpDirectory = dir_w.as_ptr();
            sei.nShow = SW_SHOWNORMAL;
            if ShellExecuteExW(&mut sei) == 0 {
                let code = GetLastError();
                if code == ERROR_CANCELLED {
                    bail!("the elevation prompt was declined");
                }
                bail!("ShellExecuteEx failed (error {code})");
            }
            if !sei.hProcess.is_null() {
                CloseHandle(sei.hProcess);
            }
        }
        Ok(())
    }

    fn place(&self, w: &LiveWindow, p: &Placement) -> Result<Option<String>> {
        let _dpi = DpiGuard::new();
        let hwnd = hwnd_of(w);
        unsafe {
            if IsWindow(hwnd) == 0 {
                bail!("window is gone");
            }
            let Some(mut wp) = placement(hwnd) else { bail!("GetWindowPlacement failed") };
            let mut r = to_win(&p.frame);
            let (ox, oy) = work_offset(&r);
            r = RECT { left: r.left - ox, top: r.top - oy, right: r.right - ox, bottom: r.bottom - oy };
            wp.rcNormalPosition = r;
            wp.flags |= WPF_ASYNCWINDOWPLACEMENT;
            wp.showCmd = match p.state {
                WindowState::Minimized => SW_SHOWMINNOACTIVE as _,
                _ => SW_SHOWNOACTIVATE as _,
            };
            if SetWindowPlacement(hwnd, &wp) == 0 {
                bail!("SetWindowPlacement failed (error {})", GetLastError());
            }
            // Again, after the window reached the right monitor (DPI change;
            // PowerToys issue #365), maximizing if needed.
            if matches!(p.state, WindowState::Zoomed | WindowState::FullScreen) {
                wp.showCmd = SW_SHOWMAXIMIZED as _;
            }
            if SetWindowPlacement(hwnd, &wp) == 0 {
                bail!("SetWindowPlacement failed (error {})", GetLastError());
            }
        }
        Ok((p.state == WindowState::FullScreen).then(|| "full screen restored as maximized".to_string()))
    }

    fn window_frame(&self, w: &LiveWindow) -> Option<Rect> {
        let _dpi = DpiGuard::new();
        unsafe {
            let hwnd = hwnd_of(w);
            if IsWindow(hwnd) == 0 {
                return None;
            }
            if IsIconic(hwnd) != 0 {
                return Some(w.frame);
            }
            let mut r: RECT = std::mem::zeroed();
            (GetWindowRect(hwnd, &mut r) != 0).then(|| to_rect(&r))
        }
    }

    fn minimize(&self, w: &LiveWindow) -> Result<()> {
        unsafe { ShowWindow(hwnd_of(w), SW_SHOWMINNOACTIVE) };
        Ok(())
    }

    fn hide_app(&self, pid: i64) -> Result<()> {
        for w in self.windows()?.into_iter().filter(|w| w.pid == pid) {
            self.minimize(&w)?;
        }
        Ok(())
    }

    fn activate(&self, w: &LiveWindow) {
        unsafe {
            let hwnd = hwnd_of(w);
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
        }
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}
