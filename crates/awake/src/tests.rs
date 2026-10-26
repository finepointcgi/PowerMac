use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, TimeZone};

use crate::*;

#[derive(Clone, Default)]
struct FakeBackend {
    log: Arc<Mutex<Vec<PowerRequest>>>,
    fail_next: Arc<Mutex<bool>>,
}
impl FakeBackend {
    fn calls(&self) -> Vec<PowerRequest> {
        self.log.lock().unwrap().clone()
    }
    fn current(&self) -> PowerRequest {
        self.calls().last().copied().unwrap_or(PowerRequest::Release)
    }
}
impl PowerBackend for FakeBackend {
    fn apply(&mut self, req: PowerRequest) -> Result<(), AwakeError> {
        if std::mem::take(&mut *self.fail_next.lock().unwrap()) {
            return Err(AwakeError::Backend("boom".into()));
        }
        self.log.lock().unwrap().push(req);
        Ok(())
    }
}

#[derive(Clone, Default)]
struct FakeProbe {
    alive: Arc<Mutex<Vec<u32>>>,
    bound: Option<u32>,
}
impl FakeProbe {
    fn spawn(&self, pid: u32) {
        self.alive.lock().unwrap().push(pid);
    }
    fn kill(&self, pid: u32) {
        self.alive.lock().unwrap().retain(|p| *p != pid);
    }
}
impl ProcessProbe for FakeProbe {
    fn bind(&mut self, pid: u32) -> bool {
        let ok = self.alive.lock().unwrap().contains(&pid);
        if ok {
            self.bound = Some(pid);
        }
        ok
    }
    fn is_alive(&mut self) -> bool {
        self.bound.is_none_or(|p| self.alive.lock().unwrap().contains(&p))
    }
    fn unbind(&mut self) {
        self.bound = None;
    }
}

#[derive(Clone)]
struct FakeClock(Arc<Mutex<DateTime<Local>>>);
impl FakeClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(t0())))
    }
    fn advance(&self, d: Duration) {
        let mut g = self.0.lock().unwrap();
        *g += chrono::Duration::from_std(d).unwrap();
    }
}
impl Clock for FakeClock {
    fn now(&self) -> DateTime<Local> {
        *self.0.lock().unwrap()
    }
}

fn t0() -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).single().unwrap()
}
fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}
fn engine() -> (Engine<FakeBackend, FakeProbe>, FakeBackend, FakeProbe) {
    let b = FakeBackend::default();
    let p = FakeProbe::default();
    (Engine::new(b.clone(), p.clone()), b, p)
}

// ---------- pure logic ----------

#[test]
fn deadlines() {
    let now = t0();
    assert_eq!(engine::compute_deadline(&AwakeMode::Passive, now).unwrap(), None);
    assert_eq!(engine::compute_deadline(&AwakeMode::Indefinite, now).unwrap(), None);
    assert_eq!(
        engine::compute_deadline(&AwakeMode::Timed(secs(90)), now).unwrap(),
        Some(now + chrono::Duration::seconds(90))
    );
    let later = now + chrono::Duration::hours(2);
    assert_eq!(engine::compute_deadline(&AwakeMode::Expirable(later), now).unwrap(), Some(later));
    assert!(matches!(
        engine::compute_deadline(&AwakeMode::Expirable(now), now),
        Err(AwakeError::ExpiryInPast(_))
    ));
    assert!(matches!(
        engine::compute_deadline(&AwakeMode::Timed(Duration::MAX), now),
        Err(AwakeError::DurationOverflow)
    ));
}

#[test]
fn request_mapping() {
    assert_eq!(engine::request_for(&AwakeConfig::passive()), PowerRequest::Release);
    assert_eq!(engine::request_for(&AwakeConfig::indefinite(false)), PowerRequest::System);
    assert_eq!(engine::request_for(&AwakeConfig::indefinite(true)), PowerRequest::SystemAndDisplay);
}

#[test]
fn timed_expires_and_reverts() {
    let (mut e, b, _) = engine();
    let now = t0();
    e.set(AwakeConfig::timed(secs(60), true), now).unwrap();
    assert_eq!(b.current(), PowerRequest::SystemAndDisplay);
    let st = e.status(now + chrono::Duration::seconds(20));
    assert!(st.active);
    assert_eq!(st.remaining, Some(secs(40)));
    assert_eq!(e.next_wake(now + chrono::Duration::seconds(20)), Some(secs(40)));

    assert_eq!(e.tick(now + chrono::Duration::seconds(59)), None);
    assert_eq!(e.tick(now + chrono::Duration::seconds(60)), Some(EndReason::TimerElapsed));
    assert_eq!(b.current(), PowerRequest::Release);
    let st = e.status(now + chrono::Duration::seconds(61));
    assert_eq!(st.mode, AwakeMode::Passive);
    assert!(!st.active);
    assert_eq!(st.remaining, None);
    assert_eq!(st.last_end, Some(EndReason::TimerElapsed));
    assert_eq!(e.next_wake(now), None);
    // Further ticks are no-ops.
    assert_eq!(e.tick(now + chrono::Duration::hours(1)), None);
}

#[test]
fn expirable_expires() {
    let (mut e, b, _) = engine();
    let now = t0();
    let at = now + chrono::Duration::minutes(5);
    e.set(AwakeConfig::expirable(at, false), now).unwrap();
    assert_eq!(b.current(), PowerRequest::System);
    assert_eq!(e.status(now).expires_at, Some(at));
    assert_eq!(e.tick(at), Some(EndReason::Expired));
    assert_eq!(b.current(), PowerRequest::Release);
}

#[test]
fn expirable_in_past_is_error_and_passive() {
    let (mut e, b, _) = engine();
    let now = t0();
    e.set(AwakeConfig::indefinite(false), now).unwrap();
    let err = e.set(AwakeConfig::expirable(now - chrono::Duration::seconds(1), false), now);
    assert!(matches!(err, Err(AwakeError::ExpiryInPast(_))));
    assert_eq!(b.current(), PowerRequest::Release);
    assert!(!e.status(now).active);
}

#[test]
fn indefinite_never_expires() {
    let (mut e, b, _) = engine();
    e.set(AwakeConfig::indefinite(false), t0()).unwrap();
    assert_eq!(e.next_wake(t0()), None);
    assert_eq!(e.tick(t0() + chrono::Duration::days(365)), None);
    assert_eq!(b.current(), PowerRequest::System);
    e.stop();
    assert_eq!(b.current(), PowerRequest::Release);
    assert_eq!(e.status(t0()).last_end, Some(EndReason::Stopped));
}

#[test]
fn changing_mode_replaces_assertion() {
    let (mut e, b, _) = engine();
    let now = t0();
    e.set(AwakeConfig::indefinite(true), now).unwrap();
    e.set(AwakeConfig::timed(secs(10), false), now).unwrap();
    assert_eq!(b.calls(), vec![PowerRequest::SystemAndDisplay, PowerRequest::System]);
    let st = e.status(now);
    assert_eq!(st.mode, AwakeMode::Timed(secs(10)));
    assert!(!st.keep_display_on);
    // The new timer, not the old indefinite mode, governs.
    assert_eq!(e.tick(now + chrono::Duration::seconds(10)), Some(EndReason::TimerElapsed));
    // Passive via set() releases.
    e.set(AwakeConfig::indefinite(false), now).unwrap();
    e.set(AwakeConfig::passive(), now).unwrap();
    assert_eq!(b.current(), PowerRequest::Release);
}

#[test]
fn pid_bound_ends_when_process_exits() {
    let (mut e, b, p) = engine();
    p.spawn(42);
    let now = t0();
    e.set(AwakeConfig::indefinite(false).with_pid(42), now).unwrap();
    assert_eq!(e.status(now).bound_pid, Some(42));
    assert_eq!(e.next_wake(now), Some(e.pid_poll_interval));
    assert_eq!(e.tick(now), None);
    p.kill(42);
    assert_eq!(e.tick(now), Some(EndReason::ProcessExited));
    assert_eq!(b.current(), PowerRequest::Release);
    assert_eq!(e.status(now).bound_pid, None);
}

#[test]
fn pid_and_timer_whichever_first() {
    let (mut e, _, p) = engine();
    p.spawn(7);
    let now = t0();
    e.set(AwakeConfig::timed(secs(5), false).with_pid(7), now).unwrap();
    // next wake is min(remaining, poll interval)
    assert_eq!(e.next_wake(now + chrono::Duration::milliseconds(4500)), Some(Duration::from_millis(500)));
    assert_eq!(e.tick(now + chrono::Duration::seconds(5)), Some(EndReason::TimerElapsed));
}

#[test]
fn missing_pid_is_error() {
    let (mut e, b, _) = engine();
    let r = e.set(AwakeConfig::indefinite(false).with_pid(999), t0());
    assert!(matches!(r, Err(AwakeError::ProcessNotFound(999))));
    assert!(b.calls().is_empty());
}

#[test]
fn backend_failure_leaves_passive() {
    let (mut e, b, _) = engine();
    *b.fail_next.lock().unwrap() = true;
    assert!(e.set(AwakeConfig::indefinite(false), t0()).is_err());
    assert!(!e.status(t0()).active);
    assert_eq!(e.status(t0()).mode, AwakeMode::Passive);
}

#[test]
fn drop_releases() {
    let (mut e, b, _) = engine();
    e.set(AwakeConfig::indefinite(true), t0()).unwrap();
    drop(e);
    assert_eq!(b.current(), PowerRequest::Release);
}

#[test]
fn parse_expire_at_formats() {
    let now = t0(); // 12:00 local
    let a = parse_expire_at("13:30", now).unwrap();
    assert_eq!(a, Local.with_ymd_and_hms(2026, 10, 25, 13, 30, 0).single().unwrap());
    let b = parse_expire_at("11:00", now).unwrap(); // rolls to tomorrow
    assert_eq!(b, Local.with_ymd_and_hms(2026, 10, 26, 11, 0, 0).single().unwrap());
    let c = parse_expire_at("2026-10-25T20:00:00Z", now).unwrap();
    assert_eq!(c, chrono::Utc.with_ymd_and_hms(2026, 10, 25, 20, 0, 0).unwrap());
    let d = parse_expire_at("2026-12-01 08:15", now).unwrap();
    assert_eq!(d, Local.with_ymd_and_hms(2026, 12, 1, 8, 15, 0).single().unwrap());
    assert!(parse_expire_at("soon", now).is_err());
}

// ---------- serde ----------

#[test]
fn config_serde_roundtrip() {
    let at = t0();
    for cfg in [
        AwakeConfig::passive(),
        AwakeConfig::indefinite(true),
        AwakeConfig::timed(secs(3600), false).with_pid(123),
        AwakeConfig::timed(Duration::from_millis(1500), false),
        AwakeConfig::expirable(at, true),
    ] {
        let s = serde_json::to_string(&cfg).unwrap();
        let back: AwakeConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back, cfg, "{s}");
    }
    let j = serde_json::to_value(AwakeConfig::timed(secs(90), true)).unwrap();
    assert_eq!(
        j,
        serde_json::json!({"mode":{"type":"timed","value":90},"keepDisplayOn":true,"boundPid":null})
    );
    let partial: AwakeConfig = serde_json::from_str(r#"{"mode":{"type":"indefinite"}}"#).unwrap();
    assert_eq!(partial, AwakeConfig::indefinite(false));
}

#[test]
fn powertoys_properties_compat() {
    let j = r#"{"keepDisplayOn":true,"mode":2,"intervalHours":1,"intervalMinutes":30,
               "expirationDateTime":"2026-10-25T18:00:00-07:00","customTrayTimes":{}}"#;
    let p: PowerToysAwakeProperties = serde_json::from_str(j).unwrap();
    let cfg = p.to_config();
    assert_eq!(cfg, AwakeConfig::timed(secs(5400), true));
    assert_eq!(PowerToysAwakeProperties::from_config(&cfg).to_config(), cfg);

    let mut p3 = p.clone();
    p3.mode = 3;
    let c3 = p3.to_config();
    assert_eq!(
        c3.mode,
        AwakeMode::Expirable(chrono::Utc.with_ymd_and_hms(2026, 10, 26, 1, 0, 0).unwrap().with_timezone(&Local))
    );
    p3.mode = 1;
    assert_eq!(p3.to_config().mode, AwakeMode::Indefinite);
    p3.mode = 0;
    assert_eq!(p3.to_config().mode, AwakeMode::Passive);
}

// ---------- threaded controller ----------

fn controller(clock: FakeClock) -> (Awake, FakeBackend, FakeProbe) {
    let b = FakeBackend::default();
    let p = FakeProbe::default();
    let (b2, p2) = (b.clone(), p.clone());
    let a = Awake::with_parts_poll(move || Ok((b2, p2)), clock, Duration::from_millis(20)).unwrap();
    (a, b, p)
}

#[test]
fn controller_fake_clock_expiry() {
    let clock = FakeClock::new();
    let (a, b, _) = controller(clock.clone());
    a.set(AwakeConfig::timed(secs(600), false)).unwrap();
    assert!(a.status().active);
    assert_eq!(a.status().remaining, Some(secs(600)));
    clock.advance(secs(300));
    assert_eq!(a.status().remaining, Some(secs(300)));
    clock.advance(secs(300));
    let st = a.status(); // tick runs before the status reply
    assert!(!st.active);
    assert_eq!(st.last_end, Some(EndReason::TimerElapsed));
    assert_eq!(b.current(), PowerRequest::Release);
}

#[test]
fn controller_real_time_timer_and_pid() {
    let (a, b, p) = controller_real();
    a.set(AwakeConfig::timed(Duration::from_millis(100), true)).unwrap();
    assert_eq!(b.current(), PowerRequest::SystemAndDisplay);
    std::thread::sleep(Duration::from_millis(300));
    // Worker reverted on its own, without any command.
    assert_eq!(b.current(), PowerRequest::Release);

    p.spawn(5);
    a.set(AwakeConfig::indefinite(false).with_pid(5)).unwrap();
    assert_eq!(b.current(), PowerRequest::System);
    p.kill(5);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(b.current(), PowerRequest::Release);
    assert_eq!(a.status().last_end, Some(EndReason::ProcessExited));

    assert!(matches!(
        a.set(AwakeConfig::indefinite(false).with_pid(6)),
        Err(AwakeError::ProcessNotFound(6))
    ));
}

fn controller_real() -> (Awake, FakeBackend, FakeProbe) {
    let b = FakeBackend::default();
    let p = FakeProbe::default();
    let (b2, p2) = (b.clone(), p.clone());
    let a = Awake::with_parts_poll(move || Ok((b2, p2)), SystemClock, Duration::from_millis(20)).unwrap();
    (a, b, p)
}

#[test]
fn controller_stop_and_drop_release() {
    let (a, b, _) = controller(FakeClock::new());
    a.set(AwakeConfig::indefinite(true)).unwrap();
    a.stop();
    assert_eq!(b.current(), PowerRequest::Release);
    a.set(AwakeConfig::indefinite(true)).unwrap();
    drop(a);
    assert_eq!(b.current(), PowerRequest::Release);
}

#[test]
fn controller_factory_error() {
    let r = Awake::with_parts(
        || Err::<(FakeBackend, FakeProbe), _>(AwakeError::Backend("nope".into())),
        SystemClock,
    );
    assert!(r.is_err());
}

#[test]
fn awake_is_send_sync() {
    fn check<T: Send + Sync>() {}
    check::<Awake>();
}

#[cfg(target_os = "macos")]
#[test]
fn mac_probe_self_and_dead() {
    let mut p = crate::platform::macos::MacProcessProbe::default();
    assert!(p.bind(std::process::id()));
    assert!(p.is_alive());
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap(); // reaped -> gone
    assert!(!p.bind(pid) || !p.is_alive());
    assert!(p.bind(1)); // launchd: EPERM counts as alive
}

/// Real IOKit round trip, checked through `pmset -g assertions` (read-only).
/// Run with `cargo test -p awake -- --ignored mac_pmset`.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn mac_pmset_roundtrip() {
    fn ours() -> Vec<String> {
        let out = std::process::Command::new("pmset").args(["-g", "assertions"]).output().unwrap();
        let me = format!("pid {}(", std::process::id());
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains(&me) && l.contains("PowerMac Awake"))
            .map(|l| l.trim().to_string())
            .collect()
    }
    let has = |v: &[String], k: &str| v.iter().any(|l| l.contains(k));
    let a = Awake::new().unwrap();
    assert!(ours().is_empty());

    a.set(AwakeConfig::indefinite(true)).unwrap();
    let v = ours();
    assert!(has(&v, "PreventUserIdleSystemSleep") && has(&v, "PreventUserIdleDisplaySleep"), "{v:?}");

    // Mode change replaces: display assertion goes away, exactly one system one.
    a.set(AwakeConfig::timed(Duration::from_millis(1500), false)).unwrap();
    let v = ours();
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(has(&v, "PreventUserIdleSystemSleep"));

    // Timer expiry releases while this process is still alive.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(ours().is_empty(), "{:?}", ours());
    assert_eq!(a.status().last_end, Some(EndReason::TimerElapsed));

    // pid binding: assertion dropped once the bound child exits.
    let mut child = std::process::Command::new("sleep").arg("1").spawn().unwrap();
    a.set(AwakeConfig::indefinite(false).with_pid(child.id())).unwrap();
    assert_eq!(ours().len(), 1);
    child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(ours().is_empty(), "{:?}", ours());
    assert_eq!(a.status().last_end, Some(EndReason::ProcessExited));

    a.set(AwakeConfig::indefinite(false)).unwrap();
    assert_eq!(ours().len(), 1);
    drop(a);
    assert!(ours().is_empty());
}
