//! The in-app controller: polls the battery, runs the policy, drives the
//! helper.

use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;

use crate::config::Config;
use crate::policy::{self, Action, PolicyInput, PolicyState};
use crate::protocol::{Command, HelperReport};
use crate::scheme::Scheme;
use crate::status::{BatteryReading, BatteryStatus};

/// Everything the controller touches outside itself (real by default;
/// replaced in tests).
pub trait Environment: Send + 'static {
    fn read_battery(&mut self) -> BatteryReading;
    fn helper_installed(&mut self) -> bool;
    /// Send commands; the last response must be a `STATUS` line.
    fn send(&mut self, cmds: &[Command]) -> Result<Vec<String>>;
    /// SMC scheme detected locally (unprivileged).
    fn local_scheme(&mut self) -> Option<Scheme>;
    /// Charge limiting can work on this platform.
    fn supported(&self) -> bool;
}

/// The real environment. On macOS it talks to the installed helper at
/// `socket` (default [`crate::helper::SOCKET_PATH`]).
pub struct SystemEnvironment {
    pub socket: PathBuf,
    /// Overrides [`crate::helper_installed`] (tests / custom installs).
    pub assume_installed: Option<bool>,
    scheme_cache: Option<Option<Scheme>>,
}

impl Default for SystemEnvironment {
    fn default() -> Self {
        SystemEnvironment {
            socket: PathBuf::from(DEFAULT_SOCKET),
            assume_installed: None,
            scheme_cache: None,
        }
    }
}

const DEFAULT_SOCKET: &str = crate::installer::SOCKET_PATH;

impl SystemEnvironment {
    pub fn with_socket(socket: impl Into<PathBuf>, assume_installed: bool) -> Self {
        SystemEnvironment {
            socket: socket.into(),
            assume_installed: Some(assume_installed),
            scheme_cache: None,
        }
    }
}

impl Environment for SystemEnvironment {
    fn read_battery(&mut self) -> BatteryReading {
        crate::platform::read_battery()
    }

    fn helper_installed(&mut self) -> bool {
        self.assume_installed
            .unwrap_or_else(crate::installer::helper_installed)
    }

    #[cfg(unix)]
    fn send(&mut self, cmds: &[Command]) -> Result<Vec<String>> {
        crate::helper::send(&self.socket, cmds)
    }

    #[cfg(not(unix))]
    fn send(&mut self, _cmds: &[Command]) -> Result<Vec<String>> {
        anyhow::bail!("no helper on this platform")
    }

    fn local_scheme(&mut self) -> Option<Scheme> {
        if self.scheme_cache.is_none() {
            self.scheme_cache = Some(crate::detect_scheme().ok());
        }
        self.scheme_cache.flatten()
    }

    fn supported(&self) -> bool {
        cfg!(target_os = "macos")
    }
}

enum Msg {
    Update(Config),
    TopUp,
    Refresh,
    Stop,
}

/// Entry point.
pub struct BatteryLimit;

impl BatteryLimit {
    /// Start polling with the real system environment.
    pub fn start(cfg: Config) -> Result<Handle> {
        Self::start_with(cfg, SystemEnvironment::default())
    }

    pub fn start_with<E: Environment>(cfg: Config, env: E) -> Result<Handle> {
        let (tx, rx) = channel();
        let status = Arc::new(Mutex::new(BatteryStatus::default()));
        let mut ctl = Controller {
            cfg,
            env,
            st: PolicyState::default(),
            status: status.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("battery-limit".into())
            .spawn(move || {
                ctl.tick();
                loop {
                    let wait = Duration::from_secs(ctl.cfg.normalized().poll_seconds);
                    match rx.recv_timeout(wait) {
                        Ok(Msg::Update(c)) => ctl.cfg = c,
                        Ok(Msg::TopUp) => ctl.st.top_up = true,
                        Ok(Msg::Refresh) | Err(RecvTimeoutError::Timeout) => {}
                        Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => {
                            ctl.shutdown();
                            return;
                        }
                    }
                    ctl.tick();
                }
            })?;
        Ok(Handle {
            tx,
            status,
            thread: Some(thread),
        })
    }
}

/// Running controller. Dropping it is the same as [`Handle::stop`].
pub struct Handle {
    tx: Sender<Msg>,
    status: Arc<Mutex<BatteryStatus>>,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    /// Apply a new config (takes effect immediately).
    pub fn update(&self, cfg: Config) {
        let _ = self.tx.send(Msg::Update(cfg));
    }

    /// Charge to 100 % now, until the adapter is unplugged.
    pub fn charge_to_full_once(&self) {
        let _ = self.tx.send(Msg::TopUp);
    }

    /// Re-read the battery now (e.g. after installing the helper).
    pub fn refresh(&self) {
        let _ = self.tx.send(Msg::Refresh);
    }

    pub fn status(&self) -> BatteryStatus {
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Stop polling and give charging back to the firmware.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = self.tx.send(Msg::Stop);
            let _ = t.join();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

struct Controller<E: Environment> {
    cfg: Config,
    env: E,
    st: PolicyState,
    status: Arc<Mutex<BatteryStatus>>,
}

fn commands_for(action: Action, keep_sleep: bool) -> Vec<Command> {
    let mut v = vec![if keep_sleep {
        Command::SleepKeep
    } else {
        Command::SleepRestore
    }];
    // Order matters: never have the adapter off while charging is allowed
    // longer than necessary, and stop discharging before allowing charge.
    match action {
        Action::Charge => v.extend([Command::DischargeOff, Command::Charge]),
        Action::Inhibit => v.extend([Command::Inhibit, Command::DischargeOff]),
        Action::Discharge => v.extend([Command::Inhibit, Command::DischargeOn]),
    }
    v.push(Command::Status);
    v
}

impl<E: Environment> Controller<E> {
    fn tick(&mut self) {
        let battery = self.env.read_battery();
        let supported = self.env.supported();
        let installed = supported && self.env.helper_installed();
        let mut error = None;
        let mut report: Option<HelperReport> = None;
        if installed {
            match self
                .env
                .send(&[Command::Status])
                .and_then(|l| HelperReport::parse(&l[0]))
            {
                Ok(r) => report = Some(r),
                Err(e) => error = Some(format!("{e:#}")),
            }
        }
        let input = PolicyInput {
            percent: battery.percent.filter(|_| battery.has_battery),
            plugged: battery.plugged,
            temperature_c: battery.temperature_c,
            helper_ready: report.is_some(),
            sleeping: false,
        };
        let cfg = self.cfg.normalized();
        let local = if supported && report.is_none() {
            self.env.local_scheme()
        } else {
            None
        };
        let charge_control = match (&report, &local) {
            (Some(r), _) => Some(r.can_inhibit()),
            (None, Some(s)) => Some(s.can_inhibit()),
            _ => None,
        };
        let mut decision = policy::step(&cfg, &mut self.st, &input);
        if !supported {
            decision.action = None;
            decision.needs_helper = false;
            decision.reason = "charge limiting isn't available on this platform".into();
        } else if cfg.enabled && charge_control == Some(false) {
            // Nothing we could write: don't ask for the helper, and don't
            // send INHIBIT every poll only to get an error back.
            decision.action = if report.is_some() {
                Some(Action::Charge)
            } else {
                None
            };
            decision.needs_helper = false;
            decision.reason =
                "this Mac's firmware has no known charge-control SMC key (CHTE, CH0B/CH0C)".into();
        }

        if let (Some(action), Some(r)) = (decision.action, report.as_ref()) {
            // While disabled, only touch the helper if it's holding state.
            let needed = cfg.enabled || r.inhibited || r.discharging;
            if needed {
                match self
                    .env
                    .send(&commands_for(action, cfg.keep_limit_during_sleep))
                {
                    Ok(lines) => {
                        if let Some(r) = lines.last().and_then(|l| HelperReport::parse(l).ok()) {
                            report = Some(r);
                        }
                    }
                    Err(e) => {
                        log::warn!("battery helper: {e:#}");
                        error = Some(format!("{e:#}"));
                    }
                }
            }
        }

        let scheme = match &report {
            Some(r) => Some(r.scheme.clone()),
            None => local.map(|s| s.name()),
        };
        let status = BatteryStatus {
            battery,
            inhibited: report.as_ref().is_some_and(|r| r.inhibited),
            discharging: report.as_ref().is_some_and(|r| r.discharging),
            helper_installed: installed,
            helper_running: report.is_some(),
            needs_helper: decision.needs_helper,
            scheme,
            charge_control,
            top_up_active: self.st.top_up,
            reason: decision.reason,
            error,
            supported,
        };
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = status;
    }

    fn shutdown(&mut self) {
        if self.env.supported() && self.env.helper_installed() {
            let cmds = [
                Command::SleepRestore,
                Command::DischargeOff,
                Command::Charge,
                Command::Status,
            ];
            if let Err(e) = self.env.send(&cmds) {
                log::debug!("battery helper on stop: {e:#}");
            }
        }
        let mut s = self.status.lock().unwrap_or_else(|p| p.into_inner());
        s.inhibited = false;
        s.discharging = false;
        s.reason = "stopped".into();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::smc::FakeSmc;

    /// In-process helper core behind a fake environment.
    struct FakeEnv {
        battery: Arc<Mutex<BatteryReading>>,
        core: Option<Arc<Mutex<crate::helper::HelperCore>>>,
        sent: Arc<Mutex<Vec<Command>>>,
        local: Scheme,
    }

    impl Environment for FakeEnv {
        fn read_battery(&mut self) -> BatteryReading {
            self.battery.lock().unwrap().clone()
        }
        fn helper_installed(&mut self) -> bool {
            self.core.is_some()
        }
        fn send(&mut self, cmds: &[Command]) -> Result<Vec<String>> {
            let core = self
                .core
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no helper"))?;
            self.sent.lock().unwrap().extend_from_slice(cmds);
            Ok(cmds
                .iter()
                .map(|c| core.lock().unwrap().handle(*c))
                .collect())
        }
        fn local_scheme(&mut self) -> Option<Scheme> {
            Some(self.local)
        }
        fn supported(&self) -> bool {
            true
        }
    }

    fn tahoe() -> Scheme {
        Scheme::detect(&mut FakeSmc::preset("tahoe").unwrap()).unwrap()
    }

    fn reading(p: u8, plugged: bool) -> BatteryReading {
        BatteryReading {
            has_battery: true,
            percent: Some(p),
            plugged,
            temperature_c: Some(25.0),
            ..Default::default()
        }
    }

    fn wait_for(h: &Handle, f: impl Fn(&BatteryStatus) -> bool) -> BatteryStatus {
        for _ in 0..200 {
            let s = h.status();
            if f(&s) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("condition not reached: {:?}", h.status());
    }

    #[test]
    fn end_to_end_with_in_process_helper() {
        let battery = Arc::new(Mutex::new(reading(85, true)));
        let core = Arc::new(Mutex::new(
            crate::helper::HelperCore::new(Box::new(FakeSmc::preset("tahoe").unwrap())).unwrap(),
        ));
        let sent = Arc::new(Mutex::new(vec![]));
        let env = FakeEnv {
            battery: battery.clone(),
            core: Some(core.clone()),
            sent: sent.clone(),
            local: tahoe(),
        };
        let cfg = Config {
            enabled: true,
            ..Default::default()
        };
        let h = BatteryLimit::start_with(cfg.clone(), env).unwrap();
        let s = wait_for(&h, |s| s.inhibited);
        assert!(s.helper_running && !s.needs_helper && !s.discharging);
        assert_eq!(s.scheme.as_deref(), Some("CHTE+CHIE"));

        h.update(Config {
            discharge_to_limit: true,
            ..cfg.clone()
        });
        wait_for(&h, |s| s.discharging);

        *battery.lock().unwrap() = reading(85, true);
        h.charge_to_full_once();
        let s = wait_for(&h, |s| s.top_up_active && !s.inhibited && !s.discharging);
        assert!(s.reason.contains("100"));

        *battery.lock().unwrap() = reading(86, false);
        h.refresh();
        wait_for(&h, |s| !s.top_up_active);

        h.update(Config {
            keep_limit_during_sleep: true,
            ..cfg.clone()
        });
        *battery.lock().unwrap() = reading(90, true);
        wait_for(&h, |s| s.inhibited);
        assert!(core.lock().unwrap().report().keep_limit_during_sleep);

        h.stop();
        let r = core.lock().unwrap().report();
        assert!(
            !r.inhibited && !r.discharging && !r.keep_limit_during_sleep,
            "stop restores charging"
        );
    }

    #[test]
    fn needs_helper_when_missing_and_quiet_when_disabled() {
        let battery = Arc::new(Mutex::new(reading(90, true)));
        let sent = Arc::new(Mutex::new(vec![]));
        let env = FakeEnv {
            battery,
            core: None,
            sent: sent.clone(),
            local: tahoe(),
        };
        let h = BatteryLimit::start_with(
            Config {
                enabled: true,
                ..Default::default()
            },
            env,
        )
        .unwrap();
        let s = wait_for(&h, |s| s.needs_helper);
        assert!(!s.helper_installed && !s.inhibited);
        assert_eq!(s.scheme.as_deref(), Some("CHTE+CHIE"));
        assert_eq!(s.charge_control, Some(true));
        assert_eq!(s.battery.percent, Some(90));
        h.stop();

        // Disabled + helper idle: only STATUS is ever sent.
        let battery = Arc::new(Mutex::new(reading(90, true)));
        let core = Arc::new(Mutex::new(
            crate::helper::HelperCore::new(Box::new(FakeSmc::preset("tahoe").unwrap())).unwrap(),
        ));
        let env = FakeEnv {
            battery,
            core: Some(core),
            sent: sent.clone(),
            local: tahoe(),
        };
        let h = BatteryLimit::start_with(Config::default(), env).unwrap();
        let s = wait_for(&h, |s| s.helper_running);
        assert!(!s.needs_helper && s.reason == "limit off");
        drop(h);
        let sent = sent.lock().unwrap();
        assert!(!sent.contains(&Command::Inhibit) && !sent.contains(&Command::DischargeOn));
    }

    /// A Mac like the M4 Pro on macOS 27 this was developed on: CHIE exists
    /// but no charge-inhibit key.
    #[test]
    fn no_charge_keys_means_no_helper_prompt_and_no_inhibit() {
        let none = Scheme::detect(&mut FakeSmc::preset("none").unwrap()).unwrap();
        let battery = Arc::new(Mutex::new(reading(90, true)));
        let sent = Arc::new(Mutex::new(vec![]));
        let env = FakeEnv {
            battery: battery.clone(),
            core: None,
            sent: sent.clone(),
            local: none,
        };
        let h = BatteryLimit::start_with(
            Config {
                enabled: true,
                ..Default::default()
            },
            env,
        )
        .unwrap();
        let s = wait_for(&h, |s| s.charge_control.is_some());
        assert_eq!(s.charge_control, Some(false));
        assert!(!s.needs_helper && s.reason.contains("no known charge-control"));
        h.stop();

        let core = Arc::new(Mutex::new(
            crate::helper::HelperCore::new(Box::new(FakeSmc::preset("none").unwrap())).unwrap(),
        ));
        let env = FakeEnv {
            battery,
            core: Some(core),
            sent: sent.clone(),
            local: none,
        };
        let h = BatteryLimit::start_with(
            Config {
                enabled: true,
                discharge_to_limit: true,
                ..Default::default()
            },
            env,
        )
        .unwrap();
        let s = wait_for(&h, |s| s.helper_running);
        assert_eq!(s.charge_control, Some(false));
        assert!(s.error.is_none(), "{:?}", s.error);
        drop(h);
        assert!(!sent.lock().unwrap().contains(&Command::Inhibit));
    }

    #[test]
    fn command_order() {
        assert_eq!(
            commands_for(Action::Discharge, false),
            vec![
                Command::SleepRestore,
                Command::Inhibit,
                Command::DischargeOn,
                Command::Status
            ]
        );
        assert_eq!(
            commands_for(Action::Charge, true),
            vec![
                Command::SleepKeep,
                Command::DischargeOff,
                Command::Charge,
                Command::Status
            ]
        );
    }
}
