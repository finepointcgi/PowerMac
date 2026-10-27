//! Windows engine: pointer acceleration/speed through `SystemParametersInfo`
//! (session only, restored on stop) and button remapping through the shared
//! `WH_MOUSE_LL` hook (`pm-hook`).

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Mutex};

use anyhow::{anyhow, Result};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, LLMHF_INJECTED, SPI_GETMOUSE, SPI_GETMOUSESPEED, SPI_SETMOUSE, SPI_SETMOUSESPEED, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use crate::actions::Executor;
use crate::{button, windows_speed, ButtonAction, Config, PointerSettings, PM_INPUT_INJECTED_TAG, SELF_INJECTED_TAG};

// ---------------------------------------------------------------------------
// Pointer settings
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WinPointer {
    /// SPI_GETMOUSE: threshold1, threshold2, acceleration (0 = off).
    mouse: [i32; 3],
    /// SPI_GETMOUSESPEED: 1..=20.
    speed: u32,
}

fn read_pointer() -> Option<WinPointer> {
    unsafe {
        let mut mouse = [0i32; 3];
        let mut speed: u32 = 10;
        if SystemParametersInfoW(SPI_GETMOUSE, 0, mouse.as_mut_ptr() as *mut c_void, 0) == 0 {
            return None;
        }
        if SystemParametersInfoW(SPI_GETMOUSESPEED, 0, &mut speed as *mut u32 as *mut c_void, 0) == 0 {
            return None;
        }
        Some(WinPointer { mouse, speed })
    }
}

/// Session-only (fWinIni = 0: not written to the profile, not broadcast).
fn write_pointer(p: WinPointer) {
    unsafe {
        let mut mouse = p.mouse;
        if SystemParametersInfoW(SPI_SETMOUSE, 0, mouse.as_mut_ptr() as *mut c_void, 0) == 0 {
            log::warn!("mouse-tuning: SPI_SETMOUSE failed");
        }
        // SPI_SETMOUSESPEED takes the value itself in pvParam.
        if SystemParametersInfoW(SPI_SETMOUSESPEED, 0, p.speed as usize as *mut c_void, 0) == 0 {
            log::warn!("mouse-tuning: SPI_SETMOUSESPEED failed");
        }
    }
}

/// Desired system values for `s`, given the originals.
fn desired(s: &PointerSettings, orig: WinPointer) -> WinPointer {
    WinPointer {
        mouse: if s.disable_acceleration { [0, 0, 0] } else { orig.mouse },
        speed: s.speed.map(windows_speed).unwrap_or(orig.speed),
    }
}

#[derive(Default)]
struct PointerState {
    orig: Option<WinPointer>,
}

impl PointerState {
    fn apply(&mut self, cfg: &Config) {
        let s = if cfg.enabled { cfg.mouse } else { PointerSettings::default() };
        if s.is_noop() {
            self.restore();
            return;
        }
        if self.orig.is_none() {
            self.orig = read_pointer();
        }
        let Some(orig) = self.orig else {
            log::warn!("mouse-tuning: cannot read the pointer settings");
            return;
        };
        let want = desired(&s, orig);
        if read_pointer() != Some(want) {
            write_pointer(want);
            log::info!("mouse-tuning: pointer {want:?} (was {orig:?})");
        }
    }

    fn restore(&mut self) {
        if let Some(orig) = self.orig.take() {
            if read_pointer() != Some(orig) {
                write_pointer(orig);
                log::info!("mouse-tuning: pointer settings restored");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Button hook
// ---------------------------------------------------------------------------

struct HookCtx {
    table: Arc<Mutex<Vec<ButtonAction>>>,
    actions: mpsc::Sender<ButtonAction>,
    swallowed: u32,
}

/// The subscription; `stop` (or dropping it) unsubscribes.
struct ButtonHook {
    guard: Option<pm_hook::Guard>,
}

impl ButtonHook {
    fn start(table: Arc<Mutex<Vec<ButtonAction>>>, actions: mpsc::Sender<ButtonAction>) -> Result<Self> {
        use pm_hook::{EventMask, EventType, Subscription, Verdict};
        let mut ctx = HookCtx { table, actions, swallowed: 0 };
        let mask = EventMask::of(&[EventType::OtherMouseDown, EventType::OtherMouseUp]);
        let guard = pm_hook::subscribe(Subscription::new("mouse-tuning", pm_hook::priority::MOUSE_TUNING, mask, move |ev| {
            let Some((msg, info)) = ev.mouse() else { return Verdict::Pass };
            let injected = info.flags & LLMHF_INJECTED != 0;
            let ours = injected && (info.dwExtraInfo == PM_INPUT_INJECTED_TAG || info.dwExtraInfo == SELF_INJECTED_TAG);
            if !ours {
                if let Some((btn, down)) = decode(msg, info.mouseData) {
                    if catch_unwind(AssertUnwindSafe(|| handle(&mut ctx, btn, down))).unwrap_or(false) {
                        return Verdict::Swallow;
                    }
                }
            }
            Verdict::Pass
        }));
        let st = pm_hook::status();
        if !st.is_active() {
            drop(guard);
            return Err(anyhow!("SetWindowsHookExW(WH_MOUSE_LL) failed ({:?})", st.state));
        }
        Ok(Self { guard: Some(guard) })
    }

    fn stop(&mut self) {
        self.guard.take();
    }
}

/// Button number (see [`button`]) and down/up for a hook message.
fn decode(msg: u32, mouse_data: u32) -> Option<(u8, bool)> {
    let x = || match (mouse_data >> 16) as u16 {
        1 => Some(button::BACK),
        2 => Some(button::FORWARD),
        _ => None,
    };
    match msg {
        WM_MBUTTONDOWN => Some((button::MIDDLE, true)),
        WM_MBUTTONUP => Some((button::MIDDLE, false)),
        WM_XBUTTONDOWN => x().map(|b| (b, true)),
        WM_XBUTTONUP => x().map(|b| (b, false)),
        _ => None,
    }
}

fn handle(ctx: &mut HookCtx, btn: u8, down: bool) -> bool {
    {
        let bit = 1u32 << btn;
        if down {
            let action = ctx.table.lock().ok().and_then(|t| t.get(btn as usize).cloned()).unwrap_or_default();
            if action == ButtonAction::Default {
                return false;
            }
            ctx.swallowed |= bit;
            if action != ButtonAction::Disabled {
                let _ = ctx.actions.send(action);
            }
            true
        } else {
            let was = ctx.swallowed & bit != 0;
            ctx.swallowed &= !bit;
            was
        }
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub(crate) struct Engine {
    pointer: PointerState,
    table: Arc<Mutex<Vec<ButtonAction>>>,
    executor: Executor,
    hook: Option<ButtonHook>,
}

impl Engine {
    pub(crate) fn start(config: Config) -> Result<Self> {
        let mut e = Self {
            pointer: PointerState::default(),
            table: Arc::new(Mutex::new(config.button_table())),
            executor: Executor::start()?,
            hook: None,
        };
        e.update(config);
        Ok(e)
    }

    pub(crate) fn update(&mut self, config: Config) {
        self.pointer.apply(&config);
        if let Ok(mut t) = self.table.lock() {
            *t = config.button_table();
        }
        match (config.needs_button_hook(), self.hook.is_some()) {
            (true, false) => {
                let r = self
                    .executor
                    .sender()
                    .ok_or_else(|| anyhow!("action executor stopped"))
                    .and_then(|tx| ButtonHook::start(self.table.clone(), tx));
                match r {
                    Ok(h) => self.hook = Some(h),
                    Err(e) => log::warn!("mouse-tuning: {e:#}"),
                }
            }
            (false, true) => {
                if let Some(mut h) = self.hook.take() {
                    h.stop();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn buttons_active(&self) -> bool {
        self.hook.is_some()
    }

    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(mut h) = self.hook.take() {
            h.stop();
        }
        self.pointer.restore();
        self.executor.stop();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
