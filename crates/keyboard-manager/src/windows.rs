//! Windows backend: a `WH_KEYBOARD_LL` hook on its own message-loop thread,
//! `SendInput` for injection. This is how PowerToys Keyboard Manager works:
//! the hook swallows the physical event (returns 1) and injects the
//! replacement, tagged with [`INJECTED_TAG`] in `dwExtraInfo` so the hook
//! lets it through. Mouse Without Borders' injected events are skipped too.

#![allow(clippy::upper_case_acronyms)]

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use pm_input::vk;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    PeekMessageW, PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::engine::{Action, Outcome};
use crate::{Shared, INJECTED_TAG, MWB_TAG};

fn key_input(vk_code: u32, down: bool) -> INPUT {
    let mut flags = 0;
    if vk::is_extended_vk(vk_code) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let scan = if vk_code == crate::keys::VK_DUMMY {
        0
    } else {
        (unsafe { MapVirtualKeyW(vk_code, MAPVK_VK_TO_VSC) }) as u16
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk_code as u16,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_TAG,
            },
        },
    }
}

fn unicode_input(unit: u16, down: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE | if down { 0 } else { KEYEVENTF_KEYUP },
                time: 0,
                dwExtraInfo: INJECTED_TAG,
            },
        },
    }
}

fn text_inputs(text: &str, out: &mut Vec<INPUT>) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let special = match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    continue;
                }
                Some(vk::VK_RETURN)
            }
            '\n' => Some(vk::VK_RETURN),
            '\t' => Some(vk::VK_TAB),
            _ => None,
        };
        if let Some(k) = special {
            out.push(key_input(k, true));
            out.push(key_input(k, false));
            continue;
        }
        let mut buf = [0u16; 2];
        for &u in c.encode_utf16(&mut buf).iter() {
            out.push(unicode_input(u, true));
            out.push(unicode_input(u, false));
        }
    }
}

fn send(inputs: &[INPUT]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let n = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if n as usize != inputs.len() {
        // Typically UIPI: the foreground window is elevated.
        bail!("SendInput injected {n}/{} events", inputs.len());
    }
    Ok(())
}

fn perform(shared: Option<&Shared>, acts: &[Action]) -> Result<()> {
    let mut inputs = Vec::new();
    for a in acts {
        match a {
            Action::Key { vk, down } => inputs.push(key_input(*vk, *down)),
            Action::Text(t) => text_inputs(t, &mut inputs),
            other => {
                if let Some(s) = shared {
                    s.submit(other);
                }
            }
        }
    }
    send(&inputs)
}

pub fn inject(acts: &[Action]) -> Result<()> {
    perform(None, acts)
}

/// Synthesize Ctrl+V.
pub fn send_paste() -> Result<()> {
    send(&[
        key_input(vk::VK_LCONTROL, true),
        key_input(vk::VK_V, true),
        key_input(vk::VK_V, false),
        key_input(vk::VK_LCONTROL, false),
    ])
}

/// Exe file name of the foreground window's process.
pub fn foreground_app() -> Vec<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return Vec::new();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return Vec::new();
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return Vec::new();
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(h);
        if !ok {
            return Vec::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
        vec![name]
    }
}

pub fn has_permission(_prompt: bool) -> bool {
    true
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

thread_local! {
    static HOOK_CTX: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
}

pub struct Hook {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start(shared: Arc<Shared>) -> Result<Hook> {
    let (tx, rx) = mpsc::channel::<std::result::Result<u32, String>>();
    let thread = std::thread::Builder::new()
        .name("km-kbd-hook".into())
        .spawn(move || unsafe {
            HOOK_CTX.with(|c| *c.borrow_mut() = Some(shared));
            let mut msg: MSG = zeroed();
            PeekMessageW(&mut msg, std::ptr::null_mut() as HWND, 0, 0, PM_NOREMOVE);
            let hook: HHOOK = SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_proc),
                GetModuleHandleW(std::ptr::null()),
                0,
            );
            if hook.is_null() {
                let _ = tx.send(Err("SetWindowsHookExW(WH_KEYBOARD_LL) failed".into()));
                return;
            }
            let _ = tx.send(Ok(GetCurrentThreadId()));
            while GetMessageW(&mut msg, std::ptr::null_mut() as HWND, 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            UnhookWindowsHookEx(hook);
            HOOK_CTX.with(|c| *c.borrow_mut() = None);
        })?;
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(thread_id)) => Ok(Hook {
            thread_id,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(anyhow!(e))
        }
        Err(_) => Err(anyhow!("keyboard hook thread did not start")),
    }
}

/// Returns whether to swallow the event.
fn handle(shared: &Shared, vk_code: u32, down: bool) -> bool {
    let acts = {
        let Ok(mut eng) = shared.engine.lock() else {
            return false;
        };
        match eng.process(vk_code, down, &mut foreground_app) {
            Outcome::Pass => return false,
            Outcome::Replace(a) => a,
        }
    };
    if let Err(e) = perform(Some(shared), &acts) {
        // Injection blocked (UIPI): the event is still swallowed; the engine
        // state matches what we tried to send, so later releases stay sane.
        log::warn!("keyboard-manager: {e:#}");
    }
    true
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let info = &*(lparam as *const KBDLLHOOKSTRUCT);
    if info.flags & LLKHF_INJECTED != 0
        && (info.dwExtraInfo == INJECTED_TAG || info.dwExtraInfo == MWB_TAG)
    {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let msg = wparam as u32;
    // VK_PACKET is unicode text injected by someone else; 0xFF is a dummy key.
    if matches!(msg, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP)
        && info.vkCode != vk::VK_PACKET
        && info.vkCode != crate::keys::VK_DUMMY
    {
        let down = info.flags & LLKHF_UP == 0;
        let swallow = HOOK_CTX.with(|c| {
            let c = c.borrow();
            let Some(shared) = c.as_ref() else {
                return false;
            };
            catch_unwind(AssertUnwindSafe(|| handle(shared, info.vkCode, down))).unwrap_or_else(
                |_| {
                    log::error!("keyboard-manager: hook handler panicked");
                    false
                },
            )
        });
        if swallow {
            return 1;
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}
