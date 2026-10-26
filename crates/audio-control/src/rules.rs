//! The device rules engine: pure logic, no OS calls, so it can be tested with
//! fake devices.
//!
//! The engine is fed a [`Snapshot`] of the devices and defaults every time
//! something changes and answers with the [`Action`]s to take:
//!
//! * **Priority** – on start, on a config change and whenever devices appear
//!   or disappear, the highest-priority available (and not blocked) device of
//!   each list becomes the default.
//! * **Blocked auto-switch** – when the default moves to a blocked device
//!   that has just appeared (≤ [`AUTO_SWITCH_WINDOW`]) or together with a
//!   device-list change, i.e. the OS switched on its own, it goes back to the
//!   previous device (or the best fallback). Moving to a blocked device that
//!   has been around for a while is taken as the user's own choice (Control
//!   Center, System Settings) and left alone.
//! * **Built-in mic** – with `keep_builtin_mic_with_bluetooth`, Bluetooth
//!   inputs count as blocked for the input slot and are also corrected on
//!   start/config change, so the default input stays on the built-in mic.
//! * **Manual override** – a switch through our UI ([`RulesEngine::note_manual`])
//!   is never undone; it holds until the device list next changes.
//! * When the output is switched by a rule and the system-output (alerts)
//!   device was following it (or was moved to the same blocked device), it is
//!   switched along.

use crate::types::{AudioDevice, Config, DeviceKind, DeviceMatch, RuleReason, Transport};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// How long after a device appears a default change to it counts as an
/// automatic (OS-initiated) switch.
pub const AUTO_SWITCH_WINDOW: Duration = Duration::from_secs(5);

/// The devices and defaults at one moment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub outputs: Vec<AudioDevice>,
    pub inputs: Vec<AudioDevice>,
    pub default_output: Option<String>,
    pub default_input: Option<String>,
    pub system_output: Option<String>,
}

impl Snapshot {
    pub fn devices(&self, kind: DeviceKind) -> &[AudioDevice] {
        match kind {
            DeviceKind::Input => &self.inputs,
            DeviceKind::Output | DeviceKind::SystemOutput => &self.outputs,
        }
    }

    pub fn default_uid(&self, kind: DeviceKind) -> Option<&str> {
        match kind {
            DeviceKind::Output => self.default_output.as_deref(),
            DeviceKind::Input => self.default_input.as_deref(),
            DeviceKind::SystemOutput => self.system_output.as_deref(),
        }
    }

    pub fn find(&self, kind: DeviceKind, uid: &str) -> Option<&AudioDevice> {
        self.devices(kind).iter().find(|d| d.uid == uid)
    }

    fn all_uids(&self) -> HashSet<String> {
        self.outputs.iter().chain(self.inputs.iter()).map(|d| d.uid.clone()).collect()
    }
}

/// Something the engine wants done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub kind: DeviceKind,
    pub uid: String,
    pub reason: RuleReason,
}

fn slot(kind: DeviceKind) -> usize {
    match kind {
        DeviceKind::Output => 0,
        DeviceKind::Input => 1,
        DeviceKind::SystemOutput => 2,
    }
}

pub struct RulesEngine {
    cfg: Config,
    /// Every uid of the previous snapshot; `None` before the first one.
    known: Option<HashSet<String>>,
    appeared_at: HashMap<String, Instant>,
    /// The default we last saw or set, per slot.
    last: [Option<String>; 3],
    /// Manual picks (output, input).
    manual: [Option<String>; 2],
    /// Apply the "enforce now" pass (priority + built-in mic) on the next
    /// snapshot: start and config changes.
    enforce: bool,
}

impl RulesEngine {
    pub fn new(cfg: Config) -> Self {
        RulesEngine {
            cfg,
            known: None,
            appeared_at: HashMap::new(),
            last: [None, None, None],
            manual: [None, None],
            enforce: true,
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// New config: rules are re-applied on the next snapshot.
    pub fn set_config(&mut self, cfg: Config) {
        let rules_changed = cfg.rules != self.cfg.rules
            || cfg.block_auto_switch_to != self.cfg.block_auto_switch_to
            || cfg.keep_builtin_mic_with_bluetooth != self.cfg.keep_builtin_mic_with_bluetooth
            || cfg.enabled != self.cfg.enabled;
        self.cfg = cfg;
        if rules_changed {
            self.enforce = true;
        }
    }

    /// The user picked `uid` in our UI: never undo it (until the device list
    /// changes).
    pub fn note_manual(&mut self, kind: DeviceKind, uid: &str) {
        if kind != DeviceKind::SystemOutput {
            self.manual[slot(kind)] = Some(uid.to_string());
        }
        self.last[slot(kind)] = Some(uid.to_string());
    }

    /// We (or a failed attempt) left `kind` on `uid`.
    pub fn note_applied(&mut self, kind: DeviceKind, uid: Option<&str>) {
        self.last[slot(kind)] = uid.map(str::to_string);
    }

    pub fn last_default(&self, kind: DeviceKind) -> Option<&str> {
        self.last[slot(kind)].as_deref()
    }

    pub fn manual(&self, kind: DeviceKind) -> Option<&str> {
        match kind {
            DeviceKind::SystemOutput => None,
            _ => self.manual[slot(kind)].as_deref(),
        }
    }

    fn priority_list(&self, kind: DeviceKind) -> &[DeviceMatch] {
        match kind {
            DeviceKind::Input => &self.cfg.rules.inputs,
            _ => &self.cfg.rules.outputs,
        }
    }

    /// May the OS / the rules not move `kind` to `dev` automatically?
    pub fn is_blocked(&self, kind: DeviceKind, dev: &AudioDevice) -> bool {
        if self.cfg.block_auto_switch_to.iter().any(|m| m.matches(dev)) {
            return true;
        }
        kind == DeviceKind::Input
            && self.cfg.keep_builtin_mic_with_bluetooth
            && dev.transport.is_bluetooth()
    }

    /// The highest-priority available device that isn't blocked.
    pub fn best_priority<'a>(&self, kind: DeviceKind, devices: &'a [AudioDevice]) -> Option<&'a AudioDevice> {
        for m in self.priority_list(kind) {
            let found = devices
                .iter()
                .find(|d| m.matches_uid(d))
                .or_else(|| devices.iter().find(|d| m.matches_name(d)));
            if let Some(d) = found {
                if !self.is_blocked(kind, d) {
                    return Some(d);
                }
            }
        }
        None
    }

    /// Where to go instead of a blocked device.
    fn fallback<'a>(
        &self,
        kind: DeviceKind,
        devices: &'a [AudioDevice],
        prev: Option<&str>,
        avoid: &str,
    ) -> Option<&'a AudioDevice> {
        let ok = |d: &&AudioDevice| d.uid != avoid && !self.is_blocked(kind, d);
        if let Some(p) = prev {
            if let Some(d) = devices.iter().filter(ok).find(|d| d.uid == p) {
                return Some(d);
            }
        }
        if let Some(d) = self.best_priority(kind, devices) {
            if d.uid != avoid {
                return Some(d);
            }
        }
        devices
            .iter()
            .filter(ok)
            .find(|d| d.transport == Transport::BuiltIn)
            .or_else(|| devices.iter().filter(ok).find(|d| !matches!(d.transport, Transport::Virtual | Transport::Aggregate)))
            .or_else(|| devices.iter().find(ok))
    }

    fn recently_appeared(&self, uid: &str, now: Instant) -> bool {
        self.appeared_at
            .get(uid)
            .is_some_and(|t| now.saturating_duration_since(*t) <= AUTO_SWITCH_WINDOW)
    }

    /// Feed a new snapshot; returns the switches to make. The engine assumes
    /// they succeed (call [`note_applied`](Self::note_applied) with the real
    /// state if one fails).
    pub fn observe(&mut self, snap: &Snapshot, now: Instant) -> Vec<Action> {
        let all = snap.all_uids();
        let first = self.known.is_none();
        let mut devices_changed = false;
        if let Some(known) = &self.known {
            for uid in all.difference(known) {
                self.appeared_at.insert(uid.clone(), now);
                devices_changed = true;
            }
            if known.difference(&all).next().is_some() {
                devices_changed = true;
            }
        }
        self.appeared_at
            .retain(|uid, t| all.contains(uid) && now.saturating_duration_since(*t) <= AUTO_SWITCH_WINDOW * 4);
        self.known = Some(all);
        if devices_changed {
            self.manual = [None, None];
        }
        let enforce = std::mem::take(&mut self.enforce) || first;

        let mut actions = Vec::new();
        if !self.cfg.enabled {
            for kind in [DeviceKind::Output, DeviceKind::Input, DeviceKind::SystemOutput] {
                self.last[slot(kind)] = snap.default_uid(kind).map(str::to_string);
            }
            return actions;
        }

        let mut output_move: Option<(Option<String>, String)> = None;
        for kind in [DeviceKind::Output, DeviceKind::Input] {
            let devices = snap.devices(kind);
            let cur = snap.default_uid(kind);
            let prev = self.last[slot(kind)].clone();
            let manual = self.manual[slot(kind)].clone();
            let mut target: Option<(String, RuleReason)> = None;

            // 1. Priority list.
            if (enforce || devices_changed) && manual.is_none() {
                if let Some(best) = self.best_priority(kind, devices) {
                    if Some(best.uid.as_str()) != cur {
                        target = Some((best.uid.clone(), RuleReason::Priority));
                    }
                }
            }

            // 2. Blocked device / Bluetooth mic.
            if target.is_none() {
                if let Some(c) = cur {
                    let dev = snap.find(kind, c);
                    let changed = prev.as_deref() != Some(c);
                    let is_manual = manual.as_deref() == Some(c);
                    if let Some(dev) = dev.filter(|d| !is_manual && self.is_blocked(kind, d)) {
                        let in_blocklist = self.cfg.block_auto_switch_to.iter().any(|m| m.matches(dev));
                        let bt_mic = !in_blocklist;
                        let automatic = changed && (self.recently_appeared(c, now) || devices_changed);
                        // The built-in-mic rule is also enforced on start and
                        // when the option is switched on.
                        let enforce_now = bt_mic && enforce;
                        if automatic || enforce_now {
                            let prev_ok = prev.as_deref().filter(|p| *p != c);
                            if let Some(fb) = self.fallback(kind, devices, prev_ok, c) {
                                let reason = if bt_mic { RuleReason::BuiltInMic } else { RuleReason::BlockedAutoSwitch };
                                target = Some((fb.uid.clone(), reason));
                            }
                        }
                    }
                }
            }

            match target {
                Some((uid, reason)) => {
                    if kind == DeviceKind::Output {
                        output_move = Some((cur.map(str::to_string), uid.clone()));
                    }
                    self.last[slot(kind)] = Some(uid.clone());
                    actions.push(Action { kind, uid, reason });
                }
                None => self.last[slot(kind)] = cur.map(str::to_string),
            }
        }

        // 3. System output follows a rule-driven output switch.
        let sys_cur = snap.system_output.clone();
        let mut sys_target = None;
        if let Some((from, to)) = &output_move {
            if let Some(s) = &sys_cur {
                let following = from.as_deref() == Some(s.as_str());
                let blocked_auto = snap
                    .find(DeviceKind::SystemOutput, s)
                    .is_some_and(|d| self.is_blocked(DeviceKind::SystemOutput, d) && self.recently_appeared(s, now));
                if s != to && (following || blocked_auto) {
                    sys_target = Some(to.clone());
                }
            }
        }
        match sys_target {
            Some(uid) => {
                self.last[2] = Some(uid.clone());
                actions.push(Action { kind: DeviceKind::SystemOutput, uid, reason: RuleReason::FollowOutput });
            }
            None => self.last[2] = sys_cur,
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PriorityRules;

    fn dev(uid: &str, name: &str, t: Transport) -> AudioDevice {
        AudioDevice { uid: uid.into(), name: name.into(), transport: t, channels: 2, ..Default::default() }
    }

    fn speakers() -> AudioDevice {
        dev("BuiltInSpeakerDevice", "MacBook Pro Speakers", Transport::BuiltIn)
    }
    fn mic() -> AudioDevice {
        dev("BuiltInMicrophoneDevice", "MacBook Pro Microphone", Transport::BuiltIn)
    }
    fn airpods_out() -> AudioDevice {
        dev("AA-BB-CC:output", "Mitch's AirPods Pro", Transport::Bluetooth)
    }
    fn airpods_in() -> AudioDevice {
        dev("AA-BB-CC:input", "Mitch's AirPods Pro", Transport::Bluetooth)
    }
    fn dac() -> AudioDevice {
        dev("AppleUSBAudioEngine:Schiit:1", "Schiit Modi", Transport::Usb)
    }
    fn hdmi() -> AudioDevice {
        dev("HDMI-1", "LG UltraFine", Transport::Hdmi)
    }

    fn snap(outputs: Vec<AudioDevice>, inputs: Vec<AudioDevice>, out: &str, inp: &str) -> Snapshot {
        Snapshot {
            outputs,
            inputs,
            default_output: Some(out.into()),
            default_input: Some(inp.into()),
            system_output: Some(out.into()),
        }
    }

    fn uid_of(actions: &[Action], kind: DeviceKind) -> Option<&str> {
        actions.iter().find(|a| a.kind == kind).map(|a| a.uid.as_str())
    }

    #[test]
    fn device_match_by_uid_then_name() {
        let exact = DeviceMatch::uid("x", "Schiit Modi");
        assert!(exact.matches(&dac()));
        assert!(!DeviceMatch::uid("x", "Schiit").matches(&dac()));
        assert!(DeviceMatch::name("airpods").matches(&airpods_out()));
        assert!(DeviceMatch::uid("BuiltInSpeakerDevice", "").matches(&speakers()));
        assert!(!DeviceMatch::default().matches(&speakers()));
    }

    #[test]
    fn device_match_json_forms() {
        let v: Vec<DeviceMatch> = serde_json::from_str(r#"["AirPods", {"uid": "u1", "name": "DAC"}, {"name": "x"}]"#).unwrap();
        assert_eq!(v[0], DeviceMatch::name("AirPods"));
        assert_eq!(v[1], DeviceMatch::uid("u1", "DAC"));
        assert_eq!(v[2], DeviceMatch::name("x"));
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
        assert!(c.enabled && !c.per_app_volume && !c.keep_builtin_mic_with_bluetooth);
        let s = serde_json::to_string(&Config::default()).unwrap();
        assert!(s.contains("blockAutoSwitchTo") && s.contains("keepBuiltinMicWithBluetooth"));
    }

    #[test]
    fn priority_picks_highest_available() {
        let cfg = Config {
            rules: PriorityRules {
                outputs: vec![DeviceMatch::uid(dac().uid, "Schiit Modi"), DeviceMatch::name("UltraFine"), DeviceMatch::uid("BuiltInSpeakerDevice", "")],
                inputs: vec![],
            },
            ..Default::default()
        };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        // Start: only speakers + HDMI, default HDMI -> UltraFine is #2, speakers #3: stay.
        let a = e.observe(&snap(vec![speakers(), hdmi()], vec![mic()], "HDMI-1", "BuiltInMicrophoneDevice"), t0);
        assert!(a.is_empty(), "{a:?}");
        // DAC plugged in, macOS leaves HDMI: switch to the DAC.
        let a = e.observe(&snap(vec![speakers(), hdmi(), dac()], vec![mic()], "HDMI-1", "BuiltInMicrophoneDevice"), t0);
        assert_eq!(uid_of(&a, DeviceKind::Output), Some(dac().uid.as_str()));
        assert_eq!(a[0].reason, RuleReason::Priority);
        // System output was following HDMI -> follows to the DAC too.
        assert_eq!(uid_of(&a, DeviceKind::SystemOutput), Some(dac().uid.as_str()));
        // Our own switch comes back as a notification: nothing more to do.
        let mut s = snap(vec![speakers(), hdmi(), dac()], vec![mic()], &dac().uid, "BuiltInMicrophoneDevice");
        assert!(e.observe(&s, t0).is_empty());
        // DAC unplugged, macOS picks speakers -> UltraFine (#2).
        s = snap(vec![speakers(), hdmi()], vec![mic()], "BuiltInSpeakerDevice", "BuiltInMicrophoneDevice");
        let a = e.observe(&s, t0);
        assert_eq!(uid_of(&a, DeviceKind::Output), Some("HDMI-1"));
    }

    #[test]
    fn priority_falls_back_to_name_when_uid_changes() {
        let cfg = Config {
            rules: PriorityRules { outputs: vec![DeviceMatch::uid("old-port-uid", "Schiit Modi")], inputs: vec![] },
            ..Default::default()
        };
        let mut e = RulesEngine::new(cfg);
        let a = e.observe(&snap(vec![speakers(), dac()], vec![mic()], "BuiltInSpeakerDevice", "BuiltInMicrophoneDevice"), Instant::now());
        assert_eq!(uid_of(&a, DeviceKind::Output), Some(dac().uid.as_str()));
    }

    #[test]
    fn user_switch_without_device_change_is_respected() {
        let cfg = Config {
            rules: PriorityRules { outputs: vec![DeviceMatch::uid(dac().uid, "")], inputs: vec![] },
            ..Default::default()
        };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        let s = snap(vec![speakers(), dac()], vec![mic()], &dac().uid, "BuiltInMicrophoneDevice");
        assert!(e.observe(&s, t0).is_empty());
        // User picks speakers in Control Center: no device change -> keep.
        let s = snap(vec![speakers(), dac()], vec![mic()], "BuiltInSpeakerDevice", "BuiltInMicrophoneDevice");
        assert!(e.observe(&s, t0 + Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn blocked_auto_switch_reverts_to_previous() {
        let cfg = Config { block_auto_switch_to: vec![DeviceMatch::name("AirPods")], ..Default::default() };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        assert!(e.observe(&snap(vec![speakers(), hdmi()], vec![mic()], "HDMI-1", "BuiltInMicrophoneDevice"), t0).is_empty());
        // AirPods connect and macOS makes them the default output.
        let s = snap(vec![speakers(), hdmi(), airpods_out()], vec![mic(), airpods_in()], &airpods_out().uid, "BuiltInMicrophoneDevice");
        let a = e.observe(&s, t0 + Duration::from_millis(10));
        assert_eq!(uid_of(&a, DeviceKind::Output), Some("HDMI-1"), "{a:?}");
        assert_eq!(a[0].reason, RuleReason::BlockedAutoSwitch);
        assert_eq!(uid_of(&a, DeviceKind::SystemOutput), Some("HDMI-1"));
    }

    #[test]
    fn blocked_switch_in_two_notifications_still_reverts() {
        // The device-list notification arrives first, the default change a
        // moment later in a separate snapshot.
        let cfg = Config { block_auto_switch_to: vec![DeviceMatch::name("AirPods")], ..Default::default() };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        e.observe(&snap(vec![speakers()], vec![mic()], "BuiltInSpeakerDevice", "BuiltInMicrophoneDevice"), t0);
        let a = e.observe(&snap(vec![speakers(), airpods_out()], vec![mic()], "BuiltInSpeakerDevice", "BuiltInMicrophoneDevice"), t0);
        assert!(a.is_empty());
        let a = e.observe(
            &snap(vec![speakers(), airpods_out()], vec![mic()], &airpods_out().uid, "BuiltInMicrophoneDevice"),
            t0 + Duration::from_millis(300),
        );
        assert_eq!(uid_of(&a, DeviceKind::Output), Some("BuiltInSpeakerDevice"));
    }

    #[test]
    fn deliberate_switch_to_blocked_device_is_allowed() {
        let cfg = Config { block_auto_switch_to: vec![DeviceMatch::name("AirPods")], ..Default::default() };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        e.observe(&snap(vec![speakers(), airpods_out()], vec![mic()], "BuiltInSpeakerDevice", "BuiltInMicrophoneDevice"), t0);
        // Long after connecting, the user picks AirPods in Control Center.
        let s = snap(vec![speakers(), airpods_out()], vec![mic()], &airpods_out().uid, "BuiltInMicrophoneDevice");
        assert!(e.observe(&s, t0 + Duration::from_secs(120)).is_empty());
    }

    #[test]
    fn manual_override_beats_block_and_priority() {
        let cfg = Config {
            rules: PriorityRules { outputs: vec![DeviceMatch::uid("HDMI-1", "")], inputs: vec![] },
            block_auto_switch_to: vec![DeviceMatch::name("AirPods")],
            ..Default::default()
        };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        e.observe(&snap(vec![speakers(), hdmi()], vec![mic()], "HDMI-1", "BuiltInMicrophoneDevice"), t0);
        let s = snap(vec![speakers(), hdmi(), airpods_out()], vec![mic()], &airpods_out().uid, "BuiltInMicrophoneDevice");
        // A manual pick made before a device change does not survive it: the
        // change ends manual mode and priority applies.
        e.note_manual(DeviceKind::Output, "BuiltInSpeakerDevice");
        let a = e.observe(&s, t0);
        assert_eq!(uid_of(&a, DeviceKind::Output), Some("HDMI-1"));
        // Typical case: devices settled, then manual switch to speakers.
        let s = snap(vec![speakers(), hdmi(), airpods_out()], vec![mic()], "HDMI-1", "BuiltInMicrophoneDevice");
        e.observe(&s, t0);
        e.note_manual(DeviceKind::Output, &airpods_out().uid);
        let s = snap(vec![speakers(), hdmi(), airpods_out()], vec![mic()], &airpods_out().uid, "BuiltInMicrophoneDevice");
        assert!(e.observe(&s, t0 + Duration::from_millis(50)).is_empty());
        // Config edits re-enforce priority but still honour the manual pick.
        e.set_config(Config {
            rules: PriorityRules { outputs: vec![DeviceMatch::uid("HDMI-1", "")], inputs: vec![] },
            block_auto_switch_to: vec![DeviceMatch::name("AirPods"), DeviceMatch::name("Beats")],
            ..Default::default()
        });
        assert!(e.observe(&s, t0 + Duration::from_millis(80)).is_empty());
    }

    #[test]
    fn blocked_devices_are_skipped_by_priority() {
        let cfg = Config {
            rules: PriorityRules { outputs: vec![DeviceMatch::name("AirPods"), DeviceMatch::uid("BuiltInSpeakerDevice", "")], inputs: vec![] },
            block_auto_switch_to: vec![DeviceMatch::name("AirPods")],
            ..Default::default()
        };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        e.observe(&snap(vec![speakers(), hdmi()], vec![mic()], "HDMI-1", "BuiltInMicrophoneDevice"), t0);
        let s = snap(vec![speakers(), hdmi(), airpods_out()], vec![mic()], &airpods_out().uid, "BuiltInMicrophoneDevice");
        let a = e.observe(&s, t0);
        assert_eq!(uid_of(&a, DeviceKind::Output), Some("BuiltInSpeakerDevice"));
    }

    #[test]
    fn revert_falls_back_when_previous_device_is_gone() {
        let cfg = Config { block_auto_switch_to: vec![DeviceMatch::name("AirPods")], ..Default::default() };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        e.observe(&snap(vec![speakers(), dac()], vec![mic()], &dac().uid, "BuiltInMicrophoneDevice"), t0);
        // DAC unplugged while AirPods connect; macOS picks AirPods.
        let s = snap(vec![speakers(), airpods_out()], vec![mic()], &airpods_out().uid, "BuiltInMicrophoneDevice");
        let a = e.observe(&s, t0);
        assert_eq!(uid_of(&a, DeviceKind::Output), Some("BuiltInSpeakerDevice"));
    }

    #[test]
    fn nothing_to_revert_to_leaves_it() {
        let cfg = Config { block_auto_switch_to: vec![DeviceMatch::name("AirPods")], ..Default::default() };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        e.observe(&Snapshot::default(), t0);
        let s = Snapshot { outputs: vec![airpods_out()], default_output: Some(airpods_out().uid), ..Default::default() };
        assert!(e.observe(&s, t0).is_empty());
    }

    #[test]
    fn builtin_mic_rule_on_connect() {
        let cfg = Config { keep_builtin_mic_with_bluetooth: true, ..Default::default() };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        e.observe(&snap(vec![speakers()], vec![mic()], "BuiltInSpeakerDevice", "BuiltInMicrophoneDevice"), t0);
        // AirPods connect: output and input both move to them.
        let s = snap(vec![speakers(), airpods_out()], vec![mic(), airpods_in()], &airpods_out().uid, &airpods_in().uid);
        let a = e.observe(&s, t0);
        // Output stays on AirPods (not blocked), input goes back to the built-in mic.
        assert_eq!(uid_of(&a, DeviceKind::Output), None);
        assert_eq!(uid_of(&a, DeviceKind::Input), Some("BuiltInMicrophoneDevice"));
        assert_eq!(a[0].reason, RuleReason::BuiltInMic);
    }

    #[test]
    fn builtin_mic_rule_enforced_on_start_and_enable() {
        let s = snap(vec![speakers(), airpods_out()], vec![mic(), airpods_in()], &airpods_out().uid, &airpods_in().uid);
        let mut e = RulesEngine::new(Config { keep_builtin_mic_with_bluetooth: true, ..Default::default() });
        let a = e.observe(&s, Instant::now());
        assert_eq!(uid_of(&a, DeviceKind::Input), Some("BuiltInMicrophoneDevice"));

        let mut e = RulesEngine::new(Config::default());
        assert!(e.observe(&s, Instant::now()).is_empty());
        e.set_config(Config { keep_builtin_mic_with_bluetooth: true, ..Default::default() });
        let a = e.observe(&s, Instant::now());
        assert_eq!(uid_of(&a, DeviceKind::Input), Some("BuiltInMicrophoneDevice"));
    }

    #[test]
    fn builtin_mic_rule_respects_manual_choice() {
        let mut e = RulesEngine::new(Config { keep_builtin_mic_with_bluetooth: true, ..Default::default() });
        let t0 = Instant::now();
        let s = snap(vec![speakers(), airpods_out()], vec![mic(), airpods_in()], &airpods_out().uid, "BuiltInMicrophoneDevice");
        assert!(e.observe(&s, t0).is_empty());
        e.note_manual(DeviceKind::Input, &airpods_in().uid);
        let s = snap(vec![speakers(), airpods_out()], vec![mic(), airpods_in()], &airpods_out().uid, &airpods_in().uid);
        assert!(e.observe(&s, t0).is_empty());
        // A later deliberate choice outside our UI (long after connect) also sticks.
        let mut e = RulesEngine::new(Config { keep_builtin_mic_with_bluetooth: true, ..Default::default() });
        let s0 = snap(vec![speakers(), airpods_out()], vec![mic(), airpods_in()], &airpods_out().uid, "BuiltInMicrophoneDevice");
        e.observe(&s0, t0);
        assert!(e.observe(&s, t0 + Duration::from_secs(30)).is_empty());
    }

    #[test]
    fn builtin_mic_rule_without_builtin_mic_uses_other_wired() {
        let usb_mic = dev("usb-mic", "Yeti", Transport::Usb);
        let mut e = RulesEngine::new(Config { keep_builtin_mic_with_bluetooth: true, ..Default::default() });
        let s = snap(vec![airpods_out()], vec![usb_mic.clone(), airpods_in()], &airpods_out().uid, &airpods_in().uid);
        let a = e.observe(&s, Instant::now());
        assert_eq!(uid_of(&a, DeviceKind::Input), Some("usb-mic"));
    }

    #[test]
    fn disabled_does_nothing() {
        let cfg = Config {
            enabled: false,
            keep_builtin_mic_with_bluetooth: true,
            block_auto_switch_to: vec![DeviceMatch::name("AirPods")],
            ..Default::default()
        };
        let mut e = RulesEngine::new(cfg);
        let s = snap(vec![speakers(), airpods_out()], vec![mic(), airpods_in()], &airpods_out().uid, &airpods_in().uid);
        assert!(e.observe(&s, Instant::now()).is_empty());
    }

    #[test]
    fn system_output_not_following_is_left_alone() {
        let cfg = Config { block_auto_switch_to: vec![DeviceMatch::name("AirPods")], ..Default::default() };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        let mut s = snap(vec![speakers(), hdmi()], vec![mic()], "HDMI-1", "BuiltInMicrophoneDevice");
        s.system_output = Some("BuiltInSpeakerDevice".into());
        e.observe(&s, t0);
        let mut s = snap(vec![speakers(), hdmi(), airpods_out()], vec![mic()], &airpods_out().uid, "BuiltInMicrophoneDevice");
        s.system_output = Some("BuiltInSpeakerDevice".into());
        let a = e.observe(&s, t0);
        assert_eq!(uid_of(&a, DeviceKind::Output), Some("HDMI-1"));
        assert_eq!(uid_of(&a, DeviceKind::SystemOutput), None);
    }

    #[test]
    fn appeared_window_expires() {
        let cfg = Config { block_auto_switch_to: vec![DeviceMatch::name("AirPods")], ..Default::default() };
        let mut e = RulesEngine::new(cfg);
        let t0 = Instant::now();
        e.observe(&snap(vec![speakers()], vec![mic()], "BuiltInSpeakerDevice", "BuiltInMicrophoneDevice"), t0);
        e.observe(&snap(vec![speakers(), airpods_out()], vec![mic()], "BuiltInSpeakerDevice", "BuiltInMicrophoneDevice"), t0);
        let s = snap(vec![speakers(), airpods_out()], vec![mic()], &airpods_out().uid, "BuiltInMicrophoneDevice");
        assert!(e.observe(&s, t0 + AUTO_SWITCH_WINDOW + Duration::from_secs(1)).is_empty());
    }
}
