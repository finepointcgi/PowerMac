//! PowerMac battery helper: a root LaunchDaemon that writes the SMC charging
//! keys on behalf of the PowerMac app. See `battery_limit::helper` for the
//! safety rules.
//!
//! ```text
//! powermac-battery-helper --allowed-uid 501 [--socket PATH] [--watchdog-secs N]
//! powermac-battery-helper --fake-smc state.json --socket PATH   (test mode)
//! ```
//!
//! `--fake-smc` drives a JSON-file-backed fake SMC instead of the real one
//! (no root needed, nothing touches the hardware) and skips the system
//! sleep hook.

#[cfg(target_os = "macos")]
fn main() {
    if let Err(e) = imp::main() {
        eprintln!("powermac-battery-helper: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("powermac-battery-helper is macOS-only");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    use anyhow::{bail, Context, Result};
    use battery_limit::helper::{self, HelperCore, ServeOptions};
    use battery_limit::smc::{AppleSmc, FakeSmc, SmcBackend};

    struct StderrLogger;
    impl log::Log for StderrLogger {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                let t = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                eprintln!("{t} {} {}", r.level(), r.args());
            }
        }
        fn flush(&self) {}
    }

    struct Args {
        socket: PathBuf,
        allowed_uid: Option<u32>,
        fake: Option<PathBuf>,
        watchdog: Duration,
    }

    fn parse_args() -> Result<Args> {
        let mut a = Args {
            socket: PathBuf::from(helper::SOCKET_PATH),
            allowed_uid: None,
            fake: None,
            watchdog: helper::DEFAULT_WATCHDOG,
        };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            let mut val = || it.next().with_context(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "--socket" => a.socket = PathBuf::from(val()?),
                "--allowed-uid" => a.allowed_uid = Some(val()?.parse()?),
                "--fake-smc" => a.fake = Some(PathBuf::from(val()?)),
                "--watchdog-secs" => a.watchdog = Duration::from_secs_f64(val()?.parse()?),
                "--version" => {
                    println!("powermac-battery-helper {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                other => bail!("unknown argument {other}"),
            }
        }
        Ok(a)
    }

    type Factory = Arc<dyn Fn() -> Result<Box<dyn SmcBackend>> + Send + Sync>;

    pub fn main() -> Result<()> {
        let _ = log::set_logger(&StderrLogger);
        log::set_max_level(log::LevelFilter::Info);
        let args = parse_args()?;
        // SAFETY: plain syscalls.
        let euid = unsafe { libc::geteuid() };
        let allowed_uid = match (args.allowed_uid, &args.fake) {
            (Some(u), _) => u,
            (None, Some(_)) => unsafe { libc::getuid() },
            (None, None) => bail!("--allowed-uid is required"),
        };

        let factory: Factory = match args.fake.clone() {
            Some(p) => Arc::new(move || Ok(Box::new(FakeSmc::open(&p)?) as Box<dyn SmcBackend>)),
            None => {
                if euid != 0 {
                    log::warn!("not running as root: SMC writes will fail");
                }
                Arc::new(|| Ok(Box::new(AppleSmc::open()?) as Box<dyn SmcBackend>))
            }
        };

        // Block the termination signals in every thread; a dedicated thread
        // receives them with sigwait and restores charging before exiting.
        let set = signal_set();
        // SAFETY: valid sigset; called before any thread is spawned.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };

        {
            let f = factory.clone();
            std::panic::set_hook(Box::new(move |info| {
                eprintln!("panic: {info}; restoring default charging");
                if let Ok(mut smc) = f() {
                    let _ = helper::restore_with(smc.as_mut());
                }
                std::process::exit(70);
            }));
        }

        let core = Arc::new(Mutex::new(HelperCore::new(factory()?)?));

        {
            let core = core.clone();
            let socket = args.socket.clone();
            std::thread::Builder::new()
                .name("signals".into())
                .spawn(move || {
                    let mut sig: libc::c_int = 0;
                    // SAFETY: valid set and out-pointer.
                    unsafe { libc::sigwait(&set, &mut sig) };
                    log::info!("signal {sig}: exiting");
                    core.lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .restore("exit");
                    let _ = std::fs::remove_file(&socket);
                    std::process::exit(0);
                })?;
        }

        if args.fake.is_none() {
            sleep::spawn(core.clone());
        }

        let read_percent: Box<dyn Fn() -> Option<u8> + Send> = if args.fake.is_some() {
            Box::new(|| None)
        } else {
            Box::new(|| battery_limit::read_battery().percent)
        };
        helper::serve(
            core,
            ServeOptions {
                socket: args.socket,
                allowed_uid,
                watchdog: args.watchdog,
                read_percent,
            },
        )
    }

    fn signal_set() -> libc::sigset_t {
        // SAFETY: sigset manipulation on a local.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::sigaddset(&mut set, libc::SIGHUP);
            set
        }
    }

    /// System sleep/wake notifications (IORegisterForSystemPower).
    mod sleep {
        use super::*;
        use battery_limit::platform::macos_ffi::*;
        use std::ffi::c_void;
        use std::sync::atomic::{AtomicU32, Ordering};

        const MSG_CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
        const MSG_SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
        const MSG_SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0300;

        static CORE: OnceLock<Arc<Mutex<HelperCore>>> = OnceLock::new();
        static ROOT_PORT: AtomicU32 = AtomicU32::new(0);

        extern "C" fn callback(
            _refcon: *mut c_void,
            _service: IoObject,
            msg: u32,
            arg: *mut c_void,
        ) {
            let port = ROOT_PORT.load(Ordering::SeqCst);
            match msg {
                MSG_CAN_SYSTEM_SLEEP => unsafe {
                    IOAllowPowerChange(port, arg as isize);
                },
                MSG_SYSTEM_WILL_SLEEP => {
                    if let Some(c) = CORE.get() {
                        c.lock().unwrap_or_else(|p| p.into_inner()).on_sleep();
                    }
                    // SAFETY: acknowledging the notification we received.
                    unsafe { IOAllowPowerChange(port, arg as isize) };
                }
                MSG_SYSTEM_HAS_POWERED_ON => {
                    if let Some(c) = CORE.get() {
                        c.lock().unwrap_or_else(|p| p.into_inner()).on_wake();
                    }
                }
                _ => {}
            }
        }

        pub fn spawn(core: Arc<Mutex<HelperCore>>) {
            let _ = CORE.set(core);
            let _ = std::thread::Builder::new().name("power".into()).spawn(|| {
                let mut port: IONotificationPortRef = std::ptr::null_mut();
                let mut notifier: IoObject = 0;
                // SAFETY: out-pointers valid; callback is 'static.
                let root = unsafe {
                    IORegisterForSystemPower(
                        std::ptr::null_mut(),
                        &mut port,
                        callback,
                        &mut notifier,
                    )
                };
                if root == 0 {
                    log::error!(
                        "IORegisterForSystemPower failed; sleep safety relies on the watchdog"
                    );
                    return;
                }
                ROOT_PORT.store(root, Ordering::SeqCst);
                // SAFETY: valid notification port; run loop of this thread.
                unsafe {
                    CFRunLoopAddSource(
                        CFRunLoopGetCurrent(),
                        IONotificationPortGetRunLoopSource(port),
                        kCFRunLoopDefaultMode,
                    );
                    CFRunLoopRun();
                }
            });
        }
    }
}
