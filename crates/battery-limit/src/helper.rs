//! The privileged helper's logic (server side) and the app's client.
//!
//! The binary in `src/bin/powermac-battery-helper.rs` is a thin wrapper
//! around [`HelperCore`] + [`serve`]. Safety rules implemented here:
//!
//! * On start the SMC is put back into the firmware default (adapter on,
//!   charging allowed); the app re-sends its desired state on its next poll.
//! * Watchdog: if no command arrives for `watchdog` while charging is
//!   inhibited or the adapter is off, the default is restored (the app
//!   crashed, hung or was quit without `stop`).
//! * System sleep: the adapter is always turned back on; charging is
//!   re-enabled unless the app sent `SLEEP KEEP`.
//! * Exit (SIGTERM/SIGINT/SIGHUP, `launchctl bootout`, panic): restore.
//! * Only the uid baked into the LaunchDaemon plist (and root) may connect
//!   (`getpeereid`), the socket is owned by that uid with mode 0600, and the
//!   command set cannot name arbitrary SMC keys.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::protocol::{
    check_ok, Command, HelperReport, MAX_COMMANDS_PER_CONNECTION, MAX_LINE, PROTOCOL_VERSION,
};
use crate::scheme::{Scheme, Write as SmcWrite};
use crate::smc::{hex, SmcBackend};

pub use crate::installer::{LABEL, SOCKET_PATH};
/// Default watchdog: the app refreshes every ~30 s.
pub const DEFAULT_WATCHDOG: Duration = Duration::from_secs(5 * 60);
/// While discharging, the adapter is turned back on below this charge.
pub const DISCHARGE_FLOOR_PERCENT: u8 = 15;

/// The helper's state and the SMC it drives.
pub struct HelperCore {
    smc: Box<dyn SmcBackend>,
    scheme: Scheme,
    inhibited: bool,
    discharging: bool,
    keep_limit_during_sleep: bool,
    last_command: Instant,
}

impl HelperCore {
    /// Detect the key scheme, read the current state and restore the
    /// firmware default if anything is non-default (no writes otherwise).
    pub fn new(smc: Box<dyn SmcBackend>) -> Result<HelperCore> {
        let mut smc = smc;
        let scheme = Scheme::detect(smc.as_mut())?;
        // A key that can't be read counts as non-default, so it gets restored.
        let mut differs = |writes: Vec<SmcWrite>| {
            writes
                .iter()
                .any(|(k, v)| smc.read_key(k).map(|cur| &cur != v).unwrap_or(true))
        };
        let inhibited = differs(scheme.charging_writes(true));
        let discharging = differs(scheme.discharge_writes(false));
        let mut core = HelperCore {
            smc,
            scheme,
            inhibited,
            discharging,
            keep_limit_during_sleep: false,
            last_command: Instant::now(),
        };
        log::info!(
            "SMC scheme: {} (inhibited={inhibited} discharging={discharging})",
            scheme.name()
        );
        core.restore("startup");
        Ok(core)
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    pub fn report(&self) -> HelperReport {
        HelperReport {
            version: PROTOCOL_VERSION,
            scheme: self.scheme.name(),
            inhibited: self.inhibited,
            discharging: self.discharging,
            keep_limit_during_sleep: self.keep_limit_during_sleep,
        }
    }

    fn write_all(&mut self, writes: &[SmcWrite]) -> Result<()> {
        for (key, bytes) in writes {
            self.smc
                .write_key(key, bytes)
                .with_context(|| format!("writing SMC {key}"))?;
            log::info!("SMC {key} <- {}", hex(bytes));
        }
        Ok(())
    }

    fn set_charging(&mut self, allow: bool) -> Result<()> {
        if !allow && !self.scheme.can_inhibit() {
            bail!("charging control not supported on this Mac");
        }
        if self.inhibited == !allow {
            return Ok(());
        }
        self.write_all(&self.scheme.charging_writes(allow))?;
        self.inhibited = !allow;
        Ok(())
    }

    fn set_discharge(&mut self, on: bool) -> Result<()> {
        if on && !self.scheme.can_discharge() {
            bail!("discharge control not supported on this Mac");
        }
        if self.discharging == on {
            return Ok(());
        }
        self.write_all(&self.scheme.discharge_writes(on))?;
        self.discharging = on;
        Ok(())
    }

    /// Run one command; returns the response line.
    pub fn handle(&mut self, cmd: Command) -> String {
        self.last_command = Instant::now();
        let r = match cmd {
            Command::Ping => return format!("OK PONG {PROTOCOL_VERSION}"),
            Command::Status => Ok(()),
            Command::Charge => self.set_charging(true),
            Command::Inhibit => self.set_charging(false),
            Command::DischargeOn => self.set_discharge(true),
            Command::DischargeOff => self.set_discharge(false),
            Command::SleepKeep => {
                self.keep_limit_during_sleep = true;
                Ok(())
            }
            Command::SleepRestore => {
                self.keep_limit_during_sleep = false;
                Ok(())
            }
        };
        match r {
            Ok(()) => self.report().to_line(),
            Err(e) => {
                log::warn!("{}: {e:#}", cmd.as_str());
                format!("ERR {e:#}")
            }
        }
    }

    /// Firmware default: adapter on, charging allowed. Best effort: every
    /// write is attempted even if an earlier one fails.
    pub fn restore(&mut self, why: &str) {
        if !self.inhibited && !self.discharging {
            return;
        }
        log::info!("restoring default charging ({why})");
        let mut ok = true;
        for (key, bytes) in self.scheme.restore_writes() {
            if let Err(e) = self.smc.write_key(key, &bytes) {
                log::error!("restore: writing SMC {key}: {e:#}");
                ok = false;
            }
        }
        if ok {
            self.inhibited = false;
            self.discharging = false;
        }
    }

    /// System is about to sleep.
    pub fn on_sleep(&mut self) {
        if self.keep_limit_during_sleep {
            if let Err(e) = self.set_discharge(false) {
                log::error!("sleep: {e:#}");
            }
        } else {
            self.restore("system sleep");
        }
    }

    /// System woke: give the app a full watchdog period to re-apply.
    pub fn on_wake(&mut self) {
        self.last_command = Instant::now();
    }

    /// Restore if the app has been silent for longer than `timeout`, or
    /// turn the adapter back on if the battery got low while discharging.
    /// Returns true if something was changed.
    pub fn watchdog_tick(&mut self, timeout: Duration, percent: Option<u8>) -> bool {
        if (self.inhibited || self.discharging) && self.last_command.elapsed() > timeout {
            self.restore("watchdog: no word from the app");
            return true;
        }
        if self.discharging && percent.is_some_and(|p| p < DISCHARGE_FLOOR_PERCENT) {
            log::warn!("battery low while discharging; adapter back on");
            let _ = self.set_discharge(false);
            return true;
        }
        false
    }
}

/// Restore the firmware default with a fresh backend (for the panic hook,
/// where the core's lock may be poisoned).
pub fn restore_with(smc: &mut dyn SmcBackend) -> Result<()> {
    let scheme = Scheme::detect(smc)?;
    for (key, bytes) in scheme.restore_writes() {
        smc.write_key(key, &bytes)?;
    }
    Ok(())
}

pub struct ServeOptions {
    pub socket: PathBuf,
    /// The only non-root uid allowed to connect.
    pub allowed_uid: u32,
    pub watchdog: Duration,
    /// Reads the battery percentage for the low-battery discharge guard.
    pub read_percent: Box<dyn Fn() -> Option<u8> + Send>,
}

fn peer_uid(stream: &UnixStream) -> Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: valid socket fd and out-pointers.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        bail!("getpeereid: {}", std::io::Error::last_os_error());
    }
    Ok(uid)
}

/// Bind the socket: stale file removed, mode 0600, owned by `allowed_uid`
/// when running as root.
pub fn bind_socket(path: &Path, allowed_uid: u32) -> Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path).with_context(|| format!("removing stale {}", path.display()))?;
    }
    // Create with a restrictive umask so there is no window where the
    // socket is world-connectable.
    // SAFETY: umask is process-wide but only touched here.
    let old = unsafe { libc::umask(0o177) };
    let listener = UnixListener::bind(path);
    unsafe { libc::umask(old) };
    let listener = listener.with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    // SAFETY: plain syscalls.
    if unsafe { libc::geteuid() } == 0 {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
        if unsafe { libc::chown(c.as_ptr(), allowed_uid, 0) } != 0 {
            bail!(
                "chown {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            );
        }
    }
    Ok(listener)
}

fn handle_connection(core: &Mutex<HelperCore>, stream: UnixStream, allowed_uid: u32) -> Result<()> {
    let uid = peer_uid(&stream)?;
    let mut out = stream.try_clone()?;
    if uid != allowed_uid && uid != 0 {
        log::warn!("rejected connection from uid {uid}");
        let _ = out.write_all(b"ERR unauthorized\n");
        return Ok(());
    }
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    out.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut reader =
        BufReader::new(stream.take((MAX_LINE as u64 + 2) * MAX_COMMANDS_PER_CONNECTION as u64));
    for _ in 0..MAX_COMMANDS_PER_CONNECTION {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(e) => return Err(e.into()),
        }
        let resp = match Command::parse(&line) {
            Ok(cmd) => core.lock().unwrap_or_else(|p| p.into_inner()).handle(cmd),
            Err(e) => format!("ERR {e}"),
        };
        out.write_all(resp.as_bytes())?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// Spawn the watchdog thread.
pub fn spawn_watchdog(
    core: Arc<Mutex<HelperCore>>,
    watchdog: Duration,
    read_percent: Box<dyn Fn() -> Option<u8> + Send>,
) {
    let tick = (watchdog / 4).clamp(Duration::from_millis(100), Duration::from_secs(30));
    std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || loop {
            std::thread::sleep(tick);
            let discharging = core.lock().map(|c| c.discharging).unwrap_or(false);
            let percent = if discharging { read_percent() } else { None };
            core.lock()
                .unwrap_or_else(|p| p.into_inner())
                .watchdog_tick(watchdog, percent);
        })
        .expect("spawn watchdog");
}

/// Accept connections forever (one at a time; commands are tiny).
pub fn serve(core: Arc<Mutex<HelperCore>>, opts: ServeOptions) -> Result<()> {
    let listener = bind_socket(&opts.socket, opts.allowed_uid)?;
    log::info!(
        "listening on {} (uid {} allowed)",
        opts.socket.display(),
        opts.allowed_uid
    );
    spawn_watchdog(core.clone(), opts.watchdog, opts.read_percent);
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                if let Err(e) = handle_connection(&core, s, opts.allowed_uid) {
                    log::debug!("connection: {e:#}");
                }
            }
            Err(e) => log::warn!("accept: {e}"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Send commands on one connection; returns one response line per command.
/// `ERR` responses are returned as errors.
pub fn send(socket: &Path, cmds: &[Command]) -> Result<Vec<String>> {
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut w = stream.try_clone()?;
    let mut msg = String::new();
    for c in cmds {
        msg.push_str(c.as_str());
        msg.push('\n');
    }
    w.write_all(msg.as_bytes())?;
    let _ = w.shutdown(std::net::Shutdown::Write);
    let mut r = BufReader::new(stream);
    let mut out = Vec::with_capacity(cmds.len());
    for c in cmds {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            bail!("helper closed the connection");
        }
        check_ok(&line).map_err(|e| anyhow!("{}: {e}", c.as_str()))?;
        out.push(line.trim_end().to_string());
    }
    Ok(out)
}

/// `STATUS` round trip.
pub fn status(socket: &Path) -> Result<HelperReport> {
    let lines = send(socket, &[Command::Status])?;
    HelperReport::parse(&lines[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smc::FakeSmc;

    struct Shared(Arc<Mutex<FakeSmc>>);
    impl SmcBackend for Shared {
        fn key_info(&mut self, k: &str) -> Result<Option<crate::smc::KeyInfo>> {
            self.0.lock().unwrap().key_info(k)
        }
        fn read_key(&mut self, k: &str) -> Result<Vec<u8>> {
            self.0.lock().unwrap().read_key(k)
        }
        fn write_key(&mut self, k: &str, b: &[u8]) -> Result<()> {
            self.0.lock().unwrap().write_key(k, b)
        }
    }

    fn core(preset: &str) -> (HelperCore, Arc<Mutex<FakeSmc>>) {
        let fake = Arc::new(Mutex::new(FakeSmc::preset(preset).unwrap()));
        let c = HelperCore::new(Box::new(Shared(fake.clone()))).unwrap();
        (c, fake)
    }

    fn key(f: &Arc<Mutex<FakeSmc>>, k: &str) -> String {
        hex(&f.lock().unwrap().state.keys[k].bytes)
    }

    #[test]
    fn startup_restores_defaults() {
        let fake = Arc::new(Mutex::new(FakeSmc::preset("tahoe").unwrap()));
        fake.lock()
            .unwrap()
            .state
            .keys
            .get_mut("CHTE")
            .unwrap()
            .bytes = vec![1, 0, 0, 0];
        fake.lock()
            .unwrap()
            .state
            .keys
            .get_mut("CHIE")
            .unwrap()
            .bytes = vec![8];
        let c = HelperCore::new(Box::new(Shared(fake.clone()))).unwrap();
        assert_eq!(key(&fake, "CHTE"), "00000000");
        assert_eq!(key(&fake, "CHIE"), "00");
        assert!(!c.report().inhibited && !c.report().discharging);
    }

    #[test]
    fn startup_without_changes_writes_nothing() {
        let (_c, f) = core("tahoe");
        assert!(f.lock().unwrap().state.writes.is_empty());
        let (_c, f) = core("legacy");
        assert!(f.lock().unwrap().state.writes.is_empty());
    }

    #[test]
    fn commands_drive_keys_tahoe() {
        let (mut c, f) = core("tahoe");
        assert!(c.handle(Command::Inhibit).contains("inhibited=1"));
        assert_eq!(key(&f, "CHTE"), "01000000");
        let n = f.lock().unwrap().state.writes.len();
        c.handle(Command::Inhibit); // idempotent: no extra write
        assert_eq!(f.lock().unwrap().state.writes.len(), n);
        assert!(c.handle(Command::DischargeOn).contains("discharging=1"));
        assert_eq!(key(&f, "CHIE"), "08");
        c.handle(Command::DischargeOff);
        c.handle(Command::Charge);
        assert_eq!(key(&f, "CHTE"), "00000000");
        assert_eq!(key(&f, "CHIE"), "00");
        assert_eq!(c.handle(Command::Ping), "OK PONG 1");
    }

    #[test]
    fn commands_drive_keys_legacy() {
        let (mut c, f) = core("legacy");
        c.handle(Command::Inhibit);
        assert_eq!(
            (key(&f, "CH0B"), key(&f, "CH0C")),
            ("02".into(), "02".into())
        );
        c.handle(Command::DischargeOn);
        assert_eq!(key(&f, "CH0I"), "01");
        c.restore("test");
        assert_eq!(
            (key(&f, "CH0B"), key(&f, "CH0C"), key(&f, "CH0I")),
            ("00".into(), "00".into(), "00".into())
        );
    }

    #[test]
    fn unsupported_scheme_errors() {
        let (mut c, f) = core("none");
        assert!(c.handle(Command::Inhibit).starts_with("ERR"));
        assert!(c.handle(Command::DischargeOn).starts_with("ERR"));
        assert!(c.handle(Command::Charge).starts_with("OK"));
        assert!(f.lock().unwrap().state.writes.is_empty());
    }

    #[test]
    fn sleep_restores_unless_kept() {
        let (mut c, f) = core("tahoe");
        c.handle(Command::Inhibit);
        c.handle(Command::DischargeOn);
        c.on_sleep();
        assert_eq!(
            (key(&f, "CHTE"), key(&f, "CHIE")),
            ("00000000".into(), "00".into())
        );

        c.handle(Command::SleepKeep);
        c.handle(Command::Inhibit);
        c.handle(Command::DischargeOn);
        c.on_sleep();
        assert_eq!(key(&f, "CHTE"), "01000000", "limit kept");
        assert_eq!(key(&f, "CHIE"), "00", "adapter always back on before sleep");
    }

    #[test]
    fn watchdog_and_low_battery_guard() {
        let (mut c, f) = core("tahoe");
        c.handle(Command::Inhibit);
        assert!(!c.watchdog_tick(Duration::from_secs(60), None));
        c.last_command = Instant::now() - Duration::from_secs(120);
        assert!(c.watchdog_tick(Duration::from_secs(60), None));
        assert_eq!(key(&f, "CHTE"), "00000000");

        c.handle(Command::DischargeOn);
        assert!(!c.watchdog_tick(Duration::from_secs(60), Some(50)));
        assert!(c.watchdog_tick(Duration::from_secs(60), Some(10)));
        assert_eq!(key(&f, "CHIE"), "00");
    }
}
