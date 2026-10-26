//! Windows backend: `WH_KEYBOARD_LL` + `WH_MOUSE_LL` hooks on their own
//! message-loop thread, `ToUnicodeEx` for the typed text and `SendInput`
//! (`KEYEVENTF_UNICODE`) for the replacement.

#![allow(clippy::upper_case_acronyms)]

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, GetKeyboardLayout, SendInput, ToUnicodeEx, INPUT, INPUT_0,
    INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    VIRTUAL_KEY,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    PeekMessageW, PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_QUIT, WM_RBUTTONDOWN, WM_SYSKEYDOWN,
    WM_XBUTTONDOWN,
};

use crate::engine::{Decision, Insert, Key, Plan};
use crate::vars::SystemEnv;
use crate::{Job, Shared, INJECTED_TAG};

const VK_BACK: u32 = 0x08;
const VK_TAB: u32 = 0x09;
const VK_RETURN: u32 = 0x0D;
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_CAPITAL: u32 = 0x14;
const VK_ESCAPE: u32 = 0x1B;
const VK_LEFT: u32 = 0x25;
const VK_RIGHT: u32 = 0x27;
const VK_V: u32 = 0x56;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_LCONTROL: u32 = 0xA2;

fn key_input(vk: u32, down: bool) -> INPUT {
    let mut flags = if down { 0 } else { KEYEVENTF_KEYUP };
    if matches!(vk, 0x21..=0x28 | 0x2D | 0x2E) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk as VIRTUAL_KEY,
                wScan: 0,
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

fn press(out: &mut Vec<INPUT>, vk: u32, n: usize) {
    for _ in 0..n {
        out.push(key_input(vk, true));
        out.push(key_input(vk, false));
    }
}

fn text_inputs(text: &str, out: &mut Vec<INPUT>) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' | '\n' => press(out, VK_RETURN, 1),
            '\t' => press(out, VK_TAB, 1),
            c => {
                let mut buf = [0u16; 2];
                for &u in c.encode_utf16(&mut buf).iter() {
                    out.push(unicode_input(u, true));
                    out.push(unicode_input(u, false));
                }
            }
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
        bail!(
            "SendInput injected {n}/{} events (elevated window?)",
            inputs.len()
        );
    }
    Ok(())
}

fn perform(shared: &Shared, plan: Plan) {
    let mut inputs = Vec::new();
    press(&mut inputs, VK_RIGHT, plan.right);
    press(&mut inputs, VK_BACK, plan.backspaces);
    match plan.insert {
        Insert::Type(t) => {
            text_inputs(&t, &mut inputs);
            press(&mut inputs, VK_LEFT, plan.left);
        }
        Insert::Paste(text) => shared.submit(Job::Paste {
            text,
            left: plan.left,
        }),
    }
    if let Err(e) = send(&inputs) {
        log::warn!("text-expander: {e:#}");
    }
}

/// Ctrl+V, then Left `left` times (from the worker thread).
pub fn paste_and_move(left: usize) -> Result<()> {
    send(&[
        key_input(VK_LCONTROL, true),
        key_input(VK_V, true),
        key_input(VK_V, false),
        key_input(VK_LCONTROL, false),
    ])?;
    if left > 0 {
        std::thread::sleep(Duration::from_millis(50));
        let mut v = Vec::new();
        press(&mut v, VK_LEFT, left);
        send(&v)?;
    }
    Ok(())
}

fn down(vk: u32) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

/// Text produced by `vk` with the current modifier state, without touching
/// the dead-key state of the focused app (`ToUnicodeEx` flag 4, Windows 10
/// 1607+).
fn translate(vk: u32, scan: u32, hwnd: HWND) -> Key {
    unsafe {
        let mut state = [0u8; 256];
        for k in [VK_SHIFT, VK_CONTROL, VK_MENU] {
            if down(k) {
                state[k as usize] = 0x80;
            }
        }
        for k in [0xA0u32, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5] {
            if down(k) {
                state[k as usize] = 0x80;
            }
        }
        if GetKeyState(VK_CAPITAL as i32) & 1 != 0 {
            state[VK_CAPITAL as usize] = 1;
        }
        let tid = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
        let layout = GetKeyboardLayout(tid);
        let mut buf = [0u16; 8];
        let n = ToUnicodeEx(
            vk,
            scan,
            state.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as i32,
            4,
            layout,
        );
        if n < 0 {
            Key::Text(String::new()) // dead key
        } else {
            Key::Text(String::from_utf16_lossy(&buf[..n as usize]))
        }
    }
}

fn classify(vk: u32, scan: u32, hwnd: HWND) -> Option<Key> {
    let ctrl = down(VK_CONTROL);
    let alt = down(VK_MENU);
    let win = down(VK_LWIN) || down(VK_RWIN);
    match vk {
        // modifiers themselves: no change
        0x10..=0x12 | 0x14 | 0x5B | 0x5C | 0xA0..=0xA5 => None,
        _ if win || (ctrl && !alt) => Some(Key::Reset), // shortcuts (Ctrl+Alt = AltGr)
        VK_BACK if !ctrl && !alt => Some(Key::Backspace),
        VK_BACK => Some(Key::Reset),
        VK_RETURN => Some(Key::Text("\n".into())),
        VK_TAB => Some(Key::Text("\t".into())),
        VK_ESCAPE | 0x21..=0x28 | 0x2D | 0x2E | 0x70..=0x87 => Some(Key::Reset),
        _ => Some(translate(vk, scan, hwnd)),
    }
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

struct Ctx {
    shared: Arc<Shared>,
    hwnd: HWND,
    app: Option<Vec<String>>,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
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
        .name("te-hook".into())
        .spawn(move || unsafe {
            CTX.with(|c| {
                *c.borrow_mut() = Some(Ctx {
                    shared,
                    hwnd: std::ptr::null_mut(),
                    app: None,
                })
            });
            let mut msg: MSG = zeroed();
            PeekMessageW(&mut msg, std::ptr::null_mut() as HWND, 0, 0, PM_NOREMOVE);
            let module = GetModuleHandleW(std::ptr::null());
            let kb: HHOOK = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
            if kb.is_null() {
                let _ = tx.send(Err("SetWindowsHookExW(WH_KEYBOARD_LL) failed".into()));
                return;
            }
            let mouse: HHOOK = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
            let _ = tx.send(Ok(GetCurrentThreadId()));
            while GetMessageW(&mut msg, std::ptr::null_mut() as HWND, 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            UnhookWindowsHookEx(kb);
            if !mouse.is_null() {
                UnhookWindowsHookEx(mouse);
            }
            CTX.with(|c| *c.borrow_mut() = None);
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
        Err(_) => Err(anyhow!("text expander hook thread did not start")),
    }
}

/// Returns whether to swallow the key.
fn handle(ctx: &mut Ctx, vk: u32, scan: u32) -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    let Ok(mut ex) = ctx.shared.expander.lock() else {
        return false;
    };
    if hwnd != ctx.hwnd {
        ctx.hwnd = hwnd;
        ctx.app = None;
        ex.reset();
    }
    let Some(key) = classify(vk, scan, hwnd) else {
        return false;
    };
    let app = &mut ctx.app;
    let mut ids = || -> Vec<String> {
        app.get_or_insert_with(keyboard_manager::foreground_app)
            .clone()
    };
    if matches!(key, Key::Text(_)) && ex.excluded(&ids()) {
        ex.reset();
        return false;
    }
    match ex.key(key, &mut ids, &SystemEnv) {
        Decision::Pass => false,
        Decision::Swallow(plan) => {
            drop(ex);
            perform(&ctx.shared, plan);
            true
        }
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam as *const KBDLLHOOKSTRUCT);
        let ours = info.flags & LLKHF_INJECTED != 0 && info.dwExtraInfo == INJECTED_TAG;
        let msg = wparam as u32;
        // VK_PACKET (0xE7) is unicode text someone else injected.
        if !ours && matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN) && info.vkCode != 0xE7 {
            let swallow = CTX.with(|c| {
                let mut c = c.borrow_mut();
                let Some(ctx) = c.as_mut() else { return false };
                catch_unwind(AssertUnwindSafe(|| handle(ctx, info.vkCode, info.scanCode)))
                    .unwrap_or_else(|_| {
                        log::error!("text-expander: hook handler panicked");
                        false
                    })
            });
            if swallow {
                return 1;
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(
            wparam as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        CTX.with(|c| {
            if let Ok(c) = c.try_borrow() {
                if let Some(ctx) = c.as_ref() {
                    if let Ok(mut ex) = ctx.shared.expander.lock() {
                        ex.reset();
                    }
                }
            }
        });
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}
