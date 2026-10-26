//! Global hotkeys on macOS via Carbon's `RegisterEventHotKey`: no
//! Accessibility / Input Monitoring permission is needed and the key press is
//! consumed. Hotkey events are delivered on the main thread by the
//! application event loop (NSApplication), so registration happens there too
//! (posted to the main dispatch queue when called from another thread).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};

use super::main_thread::on_main;
use crate::Hotkey;

/// Distinguishes this crate's hotkeys from other Carbon hotkey users in the
/// same process.
const SIGNATURE: u32 = u32::from_be_bytes(*b"PMte");

const K_EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
const K_EVENT_PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const TYPE_EVENT_HOT_KEY_ID: u32 = u32::from_be_bytes(*b"hkid");
const EVENT_NOT_HANDLED_ERR: i32 = -9874;

const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;

#[repr(C)]
#[derive(Default)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

type EventHandlerProc =
    extern "C" fn(next: *mut c_void, event: *mut c_void, user: *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: EventHandlerProc,
        num_types: u32,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut *mut c_void,
    ) -> i32;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: *mut c_void,
        options: u32,
        out_ref: *mut *mut c_void,
    ) -> i32;
    fn UnregisterEventHotKey(r: *mut c_void) -> i32;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> i32;
}

type Callback = Arc<dyn Fn() + Send + Sync>;

thread_local! {
    // Main thread only.
    static HOTKEYS: RefCell<HashMap<u32, (usize, Callback)>> = RefCell::new(HashMap::new());
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

extern "C" fn handler(_next: *mut c_void, event: *mut c_void, _user: *mut c_void) -> i32 {
    let mut id = EventHotKeyID::default();
    let err = unsafe {
        GetEventParameter(
            event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyID>(),
            std::ptr::null_mut(),
            &mut id as *mut _ as *mut c_void,
        )
    };
    if err != 0 || id.signature != SIGNATURE {
        return EVENT_NOT_HANDLED_ERR;
    }
    let cb = HOTKEYS.with(|h| h.borrow().get(&id.id).map(|(_, cb)| cb.clone()));
    match cb {
        Some(cb) => {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb()));
            0
        }
        None => EVENT_NOT_HANDLED_ERR,
    }
}

fn install_handler() -> Result<()> {
    if INSTALLED.with(|i| i.get()) {
        return Ok(());
    }
    let spec = EventTypeSpec {
        event_class: K_EVENT_CLASS_KEYBOARD,
        event_kind: K_EVENT_HOT_KEY_PRESSED,
    };
    let mut out = std::ptr::null_mut();
    let err = unsafe {
        InstallEventHandler(
            GetApplicationEventTarget(),
            handler,
            1,
            &spec,
            std::ptr::null_mut(),
            &mut out,
        )
    };
    if err != 0 {
        bail!("InstallEventHandler failed ({err})");
    }
    INSTALLED.with(|i| i.set(true));
    Ok(())
}

fn register_on_main(id: u32, keycode: u32, mods: u32, cb: Callback) -> Result<()> {
    install_handler()?;
    let mut r = std::ptr::null_mut();
    let err = unsafe {
        RegisterEventHotKey(
            keycode,
            mods,
            EventHotKeyID {
                signature: SIGNATURE,
                id,
            },
            GetApplicationEventTarget(),
            0,
            &mut r,
        )
    };
    if err != 0 || r.is_null() {
        bail!(if err == -9878 {
            "already in use by another app".to_string()
        } else {
            format!("RegisterEventHotKey failed ({err})")
        });
    }
    HOTKEYS.with(|h| h.borrow_mut().insert(id, (r as usize, cb)));
    Ok(())
}

/// A registered hotkey; dropping it unregisters it.
pub struct HotkeyReg {
    id: u32,
}

impl HotkeyReg {
    pub fn register(hk: Hotkey, cb: Callback) -> Result<Self> {
        if !hk.is_set() {
            return Ok(Self { id: 0 });
        }
        let keycode = pm_input::vk::vk_to_mac(hk.code, false)
            .ok_or_else(|| anyhow!("no Mac key for VK 0x{:02X}", hk.code))?
            as u32;
        let mut mods = 0;
        if hk.win {
            mods |= CMD_KEY;
        }
        if hk.shift {
            mods |= SHIFT_KEY;
        }
        if hk.alt {
            mods |= OPTION_KEY;
        }
        if hk.ctrl {
            mods |= CONTROL_KEY;
        }
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        on_main(move || {
            let _ = tx.send(register_on_main(id, keycode, mods, cb));
        });
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => Ok(Self { id }),
            Ok(Err(e)) => Err(e.context(format!("hotkey {hk}"))),
            // The main loop isn't running yet; it registers once it is.
            Err(_) => {
                log::info!("hotkey {hk}: registration deferred until the main loop runs");
                Ok(Self { id })
            }
        }
    }

    /// Whether a hotkey with this id is currently registered (main thread only).
    #[allow(dead_code)]
    pub fn is_registered_on_main(&self) -> bool {
        HOTKEYS.with(|h| h.borrow().contains_key(&self.id))
    }
}

impl Drop for HotkeyReg {
    fn drop(&mut self) {
        let id = self.id;
        if id == 0 {
            return;
        }
        on_main(move || {
            if let Some((r, _)) = HOTKEYS.with(|h| h.borrow_mut().remove(&id)) {
                unsafe { UnregisterEventHotKey(r as *mut c_void) };
            }
        });
    }
}
