//! Windows backend: a subscriber of the shared `WH_KEYBOARD_LL` hook
//! (`pm-hook`), `SendInput` for injection. This is how PowerToys Keyboard Manager works:
//! the hook swallows the physical event (returns 1) and injects the
//! replacement, tagged with [`INJECTED_TAG`] in `dwExtraInfo` so the hook
//! lets it through. Mouse Without Borders' injected events are skipped too.

#![allow(clippy::upper_case_acronyms)]

use std::mem::size_of;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use anyhow::{bail, Result};
use pm_input::vk;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP, WM_KEYDOWN,
    WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
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

/// The Windows-style preset (the only user of this) is a no-op on Windows.
pub fn focused_role() -> Option<String> {
    None
}

pub fn has_permission(_prompt: bool) -> bool {
    true
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/// The subscription to the shared hooks; dropping it unsubscribes.
pub struct Hook {
    _guard: pm_hook::Guard,
}

pub fn start(shared: Arc<Shared>) -> Result<Hook> {
    use pm_hook::{EventMask, Subscription, Verdict};
    let guard = pm_hook::subscribe(Subscription::new(
        "keyboard-manager",
        pm_hook::priority::KEYBOARD_MANAGER,
        EventMask::KEYS,
        move |ev| {
            let Some((msg, info)) = ev.keyboard() else { return Verdict::Pass };
            if keyboard_event(&shared, msg, info) {
                Verdict::Swallow
            } else {
                Verdict::Pass
            }
        },
    ));
    let st = pm_hook::status();
    if !st.is_active() {
        drop(guard);
        bail!("SetWindowsHookExW(WH_KEYBOARD_LL) failed ({:?})", st.state);
    }
    Ok(Hook { _guard: guard })
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

/// The old `keyboard_proc` body: true = swallow.
fn keyboard_event(shared: &Shared, msg: u32, info: &KBDLLHOOKSTRUCT) -> bool {
    if info.flags & LLKHF_INJECTED != 0
        && (info.dwExtraInfo == INJECTED_TAG
            || info.dwExtraInfo == MWB_TAG
            || info.dwExtraInfo == crate::TEXT_EXPANDER_TAG)
    {
        return false;
    }
    // VK_PACKET is unicode text injected by someone else; 0xFF is a dummy key.
    if matches!(msg, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP)
        && info.vkCode != vk::VK_PACKET
        && info.vkCode != crate::keys::VK_DUMMY
    {
        let down = info.flags & LLKHF_UP == 0;
        return catch_unwind(AssertUnwindSafe(|| handle(shared, info.vkCode, down))).unwrap_or_else(|_| {
            log::error!("keyboard-manager: hook handler panicked");
            false
        });
    }
    false
}
