//! Turning a [`ButtonAction`] into concrete steps, and running them on a
//! worker thread (never inside the event tap / hook callback).

use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::keys::{parse_combo, Combo};
use crate::ButtonAction;

/// A concrete step for one action on this platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Press a key combination.
    Keys(Combo),
    /// macOS: a Dock notification (`CoreDockSendNotification`), with
    /// `open` arguments as the fallback.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Dock { notification: &'static str, fallback_open: &'static [&'static str] },
    /// A shared automation action (`keyboard_manager::automation`).
    Automation(crate::Action),
}

fn combo(mods: &[u32], key: u32) -> Step {
    Step::Keys(Combo { modifiers: mods.to_vec(), key })
}

/// The step(s) for `action`, or `None` for pass-through / swallow-only.
pub(crate) fn plan(action: &ButtonAction) -> Option<Step> {
    use pm_input::vk::*;
    let mac = cfg!(target_os = "macos");
    // Primary shortcut modifier: Command on macOS, Ctrl on Windows.
    let primary = if mac { VK_LWIN } else { VK_LCONTROL };
    Some(match action {
        ButtonAction::Default | ButtonAction::Disabled => return None,
        ButtonAction::Back if mac => combo(&[VK_LWIN], VK_OEM_4),
        ButtonAction::Forward if mac => combo(&[VK_LWIN], VK_OEM_6),
        ButtonAction::Back => combo(&[], VK_BROWSER_BACK),
        ButtonAction::Forward => combo(&[], VK_BROWSER_FORWARD),
        ButtonAction::Keystroke { keys } => match parse_combo(keys) {
            Ok(c) => Step::Keys(c),
            Err(e) => {
                log::warn!("mouse-tuning: {e}");
                return None;
            }
        },
        ButtonAction::MissionControl if mac => Step::Dock {
            notification: "com.apple.expose.awake",
            fallback_open: &["-b", "com.apple.exposelauncher"],
        },
        ButtonAction::AppExpose if mac => Step::Dock {
            notification: "com.apple.expose.front.awake",
            fallback_open: &["-b", "com.apple.exposelauncher", "--args", "2"],
        },
        ButtonAction::Launchpad if mac => Step::Dock {
            notification: "com.apple.launchpad.toggle",
            fallback_open: &["-a", "Launchpad"],
        },
        ButtonAction::ShowDesktop if mac => Step::Dock {
            notification: "com.apple.showdesktop.awake",
            fallback_open: &["-b", "com.apple.exposelauncher", "--args", "1"],
        },
        ButtonAction::MissionControl | ButtonAction::AppExpose => combo(&[VK_LWIN], VK_TAB),
        ButtonAction::Launchpad => combo(&[], VK_LWIN),
        ButtonAction::ShowDesktop => combo(&[VK_LWIN], b'D' as u32),
        ButtonAction::Copy => combo(&[primary], b'C' as u32),
        ButtonAction::Paste => combo(&[primary], b'V' as u32),
        ButtonAction::Cut => combo(&[primary], b'X' as u32),
        ButtonAction::Undo => combo(&[primary], b'Z' as u32),
        ButtonAction::Action(crate::Action::Disabled) => return None,
        ButtonAction::Action(a) => Step::Automation(a.clone()),
    })
}

/// Worker thread that performs actions.
pub(crate) struct Executor {
    tx: Option<mpsc::Sender<ButtonAction>>,
    thread: Option<JoinHandle<()>>,
}

impl Executor {
    pub(crate) fn start() -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<ButtonAction>();
        let thread = std::thread::Builder::new().name("mouse-tuning-actions".into()).spawn(move || {
            let mut backend: Option<std::sync::Arc<dyn pm_input::InputBackend>> = None;
            for action in rx {
                let Some(step) = plan(&action) else { continue };
                if let Err(e) = run(&step, &mut backend) {
                    log::warn!("mouse-tuning: {action:?}: {e:#}");
                }
            }
        })?;
        Ok(Self { tx: Some(tx), thread: Some(thread) })
    }

    /// A sender for hook callbacks (`send` never blocks). Drop every clone
    /// before [`stop`](Self::stop), which waits for the channel to close.
    pub(crate) fn sender(&self) -> Option<mpsc::Sender<ButtonAction>> {
        self.tx.clone()
    }

    pub(crate) fn stop(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(step: &Step, backend: &mut Option<std::sync::Arc<dyn pm_input::InputBackend>>) -> anyhow::Result<()> {
    match step {
        Step::Keys(c) => {
            if backend.is_none() {
                *backend = Some(pm_input::backend()?);
            }
            let b = backend.as_ref().expect("backend set above");
            press(b.as_ref(), c)
        }
        Step::Automation(a) => {
            let o = keyboard_manager::automation::run_action_once(a);
            if o.is_ok() {
                Ok(())
            } else {
                Err(anyhow::anyhow!("{}", o.summary()))
            }
        }
        Step::Dock { notification, fallback_open } => {
            #[cfg(target_os = "macos")]
            {
                if !crate::macos::dock_notification(notification) {
                    std::process::Command::new("/usr/bin/open").args(*fallback_open).spawn()?;
                }
                Ok(())
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (notification, fallback_open);
                Ok(())
            }
        }
    }
}

fn key(b: &dyn pm_input::InputBackend, vk: u32, down: bool) -> anyhow::Result<()> {
    b.inject_key(pm_input::KeyEvent { vk, down, extended: pm_input::vk::is_extended_vk(vk) })
}

/// Modifiers down, key down/up, modifiers up (reverse order). Modifiers are
/// released even when a step fails.
fn press(b: &dyn pm_input::InputBackend, c: &Combo) -> anyhow::Result<()> {
    let mut pressed = Vec::new();
    let mut result = Ok(());
    for &m in &c.modifiers {
        match key(b, m, true) {
            Ok(()) => pressed.push(m),
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    if result.is_ok() {
        result = key(b, c.key, true).and_then(|_| key(b, c.key, false));
    }
    for &m in pressed.iter().rev() {
        let r = key(b, m, false);
        if result.is_ok() {
            result = r;
        }
    }
    result
}
