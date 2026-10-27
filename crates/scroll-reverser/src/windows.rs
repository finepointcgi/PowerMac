//! Windows backend: a subscriber of the shared `WH_MOUSE_LL` hook
//! (`pm-hook`, priority [`pm_hook::priority::SCROLL_REVERSER`]).
//!
//! Low-level hooks cannot modify events, so a wheel event that must be
//! reversed is swallowed and re-injected (with `SendInput`, from a worker
//! thread rather than inside the hook callback) with the negated delta and
//! `dwExtraInfo = SELF_INJECTED_TAG`. Events tagged by us or by `pm-input`
//! pass through untouched.
//!
//! LL hooks carry no device identity, so all wheel input is classified as
//! `Device::Mouse` unless `Config::windows_touchpad_heuristic` is on.
//!
//! Smooth scrolling: a mouse wheel event is swallowed and its (possibly
//! reversed) notches are handed to a frame thread that replays them as many
//! small `SendInput` wheel deltas tagged `SELF_INJECTED_TAG`.

use std::mem::size_of;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    LLMHF_INJECTED, WM_MOUSEHWHEEL, WM_MOUSEWHEEL,
};

use crate::smooth::Animator;
use crate::{
    classify_windows_delta, windows_plan, Observer, ScrollEventInfo, SharedConfig, WinPlan,
    SELF_INJECTED_TAG, WIN_UNITS_PER_PX,
};

/// Smooth-scrolling frame interval (~125 Hz; needs the 1 ms timer period).
const FRAME: Duration = Duration::from_millis(8);

struct HookCtx {
    shared: Arc<SharedConfig>,
    observer: Option<Observer>,
    /// Re-injects reversed wheel events after the hook returned.
    reinject: pm_hook::Worker,
    animator: Option<Animator>,
}

fn timer_fine() {
    unsafe { timeBeginPeriod(1) };
}
fn timer_coarse() {
    unsafe { timeEndPeriod(1) };
}

fn modifiers_held() -> bool {
    [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
        .iter()
        .any(|&vk| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0)
}

/// The subscription; dropping it (or `stop`) unsubscribes.
pub(crate) struct Tap {
    guard: Option<pm_hook::Guard>,
}

impl Tap {
    pub(crate) fn stop(mut self) {
        self.guard.take();
    }
}

pub(crate) fn start(shared: Arc<SharedConfig>, observer: Option<Observer>) -> Result<Tap> {
    use pm_hook::{EventMask, Subscription, Verdict};
    let animator = Animator::start(
        Box::new(|d: [i64; 2]| {
            let c = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            if d[0] != 0 {
                unsafe { reinject(false, c(d[0])) };
            }
            if d[1] != 0 {
                unsafe { reinject(true, c(d[1])) };
            }
        }),
        FRAME,
        timer_fine,
        timer_coarse,
    )
    .map_err(|e| log::error!("scroll-reverser: smooth scrolling thread: {e}"))
    .ok();
    let reinject = pm_hook::Worker::new("scroll-reverser-reinject")?;
    let ctx = HookCtx { shared, observer, reinject, animator };
    let guard = pm_hook::subscribe(Subscription::new(
        "scroll-reverser",
        pm_hook::priority::SCROLL_REVERSER,
        EventMask::SCROLL,
        move |ev| {
            let Some((msg, info)) = ev.mouse() else { return Verdict::Pass };
            if msg != WM_MOUSEWHEEL && msg != WM_MOUSEHWHEEL {
                return Verdict::Pass;
            }
            let delta = (info.mouseData >> 16) as u16 as i16 as i32;
            let extra = if info.flags & LLMHF_INJECTED != 0 { info.dwExtraInfo } else { 0 };
            let swallow = catch_unwind(AssertUnwindSafe(|| handle_wheel(&ctx, msg == WM_MOUSEHWHEEL, delta, extra)))
                .unwrap_or(false);
            if swallow {
                Verdict::Swallow
            } else {
                Verdict::Pass
            }
        },
    ));
    let st = pm_hook::status();
    if !st.is_active() {
        drop(guard);
        return Err(anyhow!("SetWindowsHookExW(WH_MOUSE_LL) failed ({:?})", st.state));
    }
    Ok(Tap { guard: Some(guard) })
}

unsafe fn reinject(horizontal: bool, delta: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: delta as u32,
                dwFlags: if horizontal { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL },
                time: 0,
                dwExtraInfo: SELF_INJECTED_TAG,
            },
        },
    };
    if SendInput(1, &input, size_of::<INPUT>() as i32) != 1 {
        // Typically UIPI (elevated window under the cursor) or the secure desktop.
        log::debug!("scroll-reverser: SendInput failed for reversed wheel event");
    }
}

/// Returns true when the event should be swallowed.
fn handle_wheel(ctx: &HookCtx, horizontal: bool, delta: i32, extra: usize) -> bool {
    {
        let cfg = ctx.shared.load();
        let mut plan = windows_plan(&cfg, horizontal, delta, extra, false);
        if matches!(plan, WinPlan::Smooth(_)) && modifiers_held() {
            plan = windows_plan(&cfg, horizontal, delta, extra, true);
        }
        if let Some(obs) = &ctx.observer {
            if extra != SELF_INJECTED_TAG && extra != crate::PM_INPUT_INJECTED_TAG {
                let device = classify_windows_delta(delta, cfg.windows_touchpad_heuristic);
                let rev = cfg.reverse_for(device);
                let (v, h) = if horizontal { (0.0, delta as f64) } else { (delta as f64, 0.0) };
                obs(&ScrollEventInfo {
                    device,
                    vertical: v,
                    horizontal: h,
                    reversed: crate::Reverse {
                        vertical: !horizontal && rev.vertical,
                        horizontal: horizontal && rev.horizontal,
                    },
                    continuous: false,
                    scroll_phase: 0,
                    momentum_phase: 0,
                    smoothed: matches!(plan, WinPlan::Smooth(_)),
                });
            }
        }
        match plan {
            WinPlan::Pass => false,
            WinPlan::Reinject(d) => ctx.reinject.submit(move || unsafe { reinject(horizontal, d) }),
            WinPlan::Smooth(notches) => match &ctx.animator {
                Some(a) => {
                    let n = if horizontal { [0.0, notches] } else { [notches, 0.0] };
                    a.push(n, cfg.smooth_params(WIN_UNITS_PER_PX));
                    true
                }
                None => false,
            },
        }
    }
}
