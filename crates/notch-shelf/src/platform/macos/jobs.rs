//! The widget runner: one background thread that schedules the
//! data-driven widget instances ([`crate::widgets::scheduler`]) and runs
//! each job (a script, a plugin, a web request, `system_profiler`) on a
//! short-lived worker thread, so a slow command never holds up the others.
//! Reminders (EventKit), Sound (the host's provider) and Shortcuts runs are
//! handled here too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{bluetooth as mac_bt, reminders as mac_rem, rt};
use crate::widgets::bluetooth::{self, BtDevice};
use crate::widgets::catalog::{KindSettings, ScriptSettings, WebSettings, XbarSettings};
use crate::widgets::content::{hash_bytes, parse_output, Content, Status};
use crate::widgets::instance::WidgetInstance;
use crate::widgets::reminders::RemindersSource;
use crate::widgets::run::{self, argv_for, plugin_argv};
use crate::widgets::scheduler::{next_wake, Schedule, Visibility};
use crate::widgets::sound::SoundState;
use crate::widgets::{shortcuts, unix_now, web, xbar, Access, InstanceData, ShortcutsState, WidgetKind};
use crate::{Config, Shared, SoundCmd};

const BT_KEY: &str = "bluetooth";
const REM_KEY: &str = "reminders";
const SOUND_KEY: &str = "sound";
const WEB_CACHE_FILE: &str = "notch-web-cache.json";

pub fn spawn(shared: Arc<Shared>) {
    let _ = std::thread::Builder::new().name("notch-jobs".into()).spawn(move || run_loop(shared));
}

#[allow(clippy::large_enum_variant)]
enum Done {
    Content { id: String, result: Result<Content, String>, web: Option<WebCacheEntry> },
    Bluetooth(Result<Vec<BtDevice>, String>),
    Shortcut { id: String, name: String, result: Result<String, String> },
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct WebCacheEntry {
    /// Hash of the request (URL + headers) the body belongs to.
    key: u64,
    body: String,
    etag: Option<String>,
    at: f64,
}

fn config(s: &Shared) -> Option<Config> {
    let st = s.lock();
    (!st.stopped).then(|| st.config.clone())
}

/// What a data-driven instance needs from the runner.
struct JobSpec {
    key: String,
    kind: WidgetKind,
    settings: KindSettings,
    interval: f64,
    live: bool,
    page: usize,
    hash: u64,
}

fn job_specs(cfg: &Config) -> Vec<JobSpec> {
    let mut v = vec![];
    if !cfg.enabled {
        return v;
    }
    for w in cfg.widgets.iter().filter(|w| w.enabled) {
        let settings = KindSettings::parse(w.kind, &w.settings);
        let (interval, live) = match &settings {
            KindSettings::Script(s) => (s.interval, s.live),
            KindSettings::Web(s) => (s.interval, s.live),
            KindSettings::Xbar(s) => (xbar_interval(s), s.live),
            _ => continue,
        };
        let hash = hash_bytes(serde_json::to_string(&w.settings).unwrap_or_default().as_bytes());
        v.push(JobSpec { key: w.id.clone(), kind: w.kind, settings, interval, live, page: w.page, hash });
    }
    v
}

/// Interval of an xbar instance: its setting, else the file name's, else
/// (or for a folder) the shortest of its plugins', 5 minutes at most.
pub fn xbar_interval(s: &XbarSettings) -> f64 {
    if s.interval > 0.0 {
        return s.interval;
    }
    let p = xbar::expand_tilde(&s.path);
    let name_of = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if p.is_dir() {
        return xbar::list_plugins(&p).iter().filter_map(|f| xbar::interval_from_name(&name_of(f))).fold(300.0, f64::min);
    }
    xbar::interval_from_name(&name_of(&p)).unwrap_or(300.0)
}

fn vis(presence: &crate::Presence, page: usize, n_pages: usize, live: bool, now: f64) -> Visibility {
    let shown = presence.home && page.min(n_pages.saturating_sub(1)) == presence.page;
    Visibility { shown, panel_open: presence.panel_open, closed_for: if presence.panel_open { 0.0 } else { (now - presence.closed_at).max(0.0) }, live }
}

fn run_loop(s: Arc<Shared>) {
    let (tx, rx) = mpsc::channel::<Done>();
    let data_dir: PathBuf = s.lock().store.data_dir().to_path_buf();
    let mut scheds: HashMap<String, (Schedule, u64)> = HashMap::new();
    let mut web_cache: HashMap<String, WebCacheEntry> = std::fs::read(data_dir.join(WEB_CACHE_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut web_cache_dirty = false;
    let mut reminders: Option<mac_rem::EkReminders> = None;
    let mut rem_asked = false;
    let mut rem_requesting = false;
    let mut shown_cached = false;
    while let Some(cfg) = config(&s) {
        let now = unix_now();
        let demo = s.demo.load(std::sync::atomic::Ordering::Relaxed);
        // Finished jobs.
        while let Ok(d) = rx.try_recv() {
            match d {
                Done::Content { id, result, web } => {
                    let ok = result.is_ok();
                    if let Some((sc, _)) = scheds.get_mut(&id) {
                        sc.finish(now, ok);
                    }
                    if let Some(e) = web {
                        web_cache.insert(id.clone(), e);
                        web_cache_dirty = true;
                    }
                    if let Err(e) = &result {
                        log::info!("notch-shelf: widget {id}: {e}");
                    }
                    s.set_data(demo, |d| {
                        let prev = match d.instances.remove(&id) {
                            Some(InstanceData::Content(st)) => st,
                            _ => Status::Loading,
                        };
                        d.instances.insert(id, InstanceData::Content(prev.next(result, now)));
                    });
                }
                Done::Bluetooth(r) => {
                    if let Some((sc, _)) = scheds.get_mut(BT_KEY) {
                        sc.finish(now, r.is_ok());
                    }
                    match r {
                        Ok(devs) => s.set_data(false, |d| d.bluetooth = Some(devs)),
                        Err(e) => {
                            log::info!("notch-shelf: bluetooth: {e}");
                            if s.data().bluetooth.is_none() {
                                s.set_data(false, |d| d.bluetooth = Some(vec![]));
                            }
                        }
                    }
                }
                Done::Shortcut { id, name, result } => s.set_data(demo, |d| {
                    d.instances.insert(id, InstanceData::Shortcuts(ShortcutsState { running: None, last: Some((name, result, now)) }));
                }),
            }
        }
        if web_cache_dirty {
            web_cache_dirty = false;
            if let Ok(b) = serde_json::to_vec(&web_cache) {
                let _ = std::fs::write(data_dir.join(WEB_CACHE_FILE), b);
            }
        }
        let (refresh, rem_ask, rem_done, sound_cmds, shortcut_runs) = {
            let mut r = s.reqs();
            (std::mem::take(&mut r.refresh), std::mem::take(&mut r.reminders_ask), std::mem::take(&mut r.reminders_complete), std::mem::take(&mut r.sound), std::mem::take(&mut r.shortcuts))
        };
        let presence = s.presence();
        let n_pages = cfg.page_list().len();
        let all = refresh.iter().any(|r| r == "*");
        // ----- Script / xbar / Web ------------------------------------------
        let specs = job_specs(&cfg);
        for spec in &specs {
            let entry = scheds.entry(spec.key.clone()).or_insert_with(|| (Schedule::new(spec.interval), spec.hash));
            entry.0.set_interval(spec.interval);
            if entry.1 != spec.hash {
                entry.1 = spec.hash;
                entry.0.force();
            }
            if all || refresh.contains(&spec.key) {
                entry.0.force();
            }
            // A cached web response shows right away after a restart.
            if spec.kind == WidgetKind::Web && entry.0.last_start.is_none() && !s.data().instances.contains_key(&spec.key) {
                if let (Some(c), KindSettings::Web(ws)) = (web_cache.get(&spec.key), &spec.settings) {
                    if c.key == web_request_key(ws) {
                        if let Ok(content) = web::to_content(&c.body, ws, rt::utc_offset()) {
                            let id = spec.key.clone();
                            let at = c.at;
                            s.set_data(demo, |d| {
                                d.instances.insert(id, InstanceData::Content(Status::Ready { content, at }));
                            });
                        }
                    }
                }
            }
            let v = vis(&presence, spec.page, n_pages, spec.live, now);
            if entry.0.due(now, &v) {
                entry.0.start(now);
                let tx = tx.clone();
                let id = spec.key.clone();
                let settings = spec.settings.clone();
                let cache = web_cache.get(&spec.key).cloned();
                let sig = s.clone();
                let _ = std::thread::Builder::new().name("notch-job".into()).spawn(move || {
                    let (result, web) = run_job(&id, &settings, cache);
                    let _ = tx.send(Done::Content { id, result, web });
                    sig.signal.notify();
                });
            }
        }
        // ----- Bluetooth -------------------------------------------------------
        let bt: Vec<&WidgetInstance> = if cfg.enabled { cfg.instances_of(WidgetKind::Bluetooth).collect() } else { vec![] };
        if !bt.is_empty() && !demo {
            let shown = bt.iter().any(|w| vis(&presence, w.page, n_pages, false, now).shown);
            let entry = scheds.entry(BT_KEY.into()).or_insert_with(|| (Schedule::new(60.0), 0));
            entry.0.set_interval(if shown { 30.0 } else { 60.0 });
            if all || refresh.iter().any(|r| bt.iter().any(|w| &w.id == r)) {
                entry.0.force();
            }
            let v = Visibility { shown, ..vis(&presence, usize::MAX, 0, false, now) };
            if entry.0.due(now, &v) {
                entry.0.start(now);
                let tx = tx.clone();
                let sig = s.clone();
                let _ = std::thread::Builder::new().name("notch-bt".into()).spawn(move || {
                    let _ = tx.send(Done::Bluetooth(read_bluetooth()));
                    sig.signal.notify();
                });
            }
        }
        // ----- Reminders -----------------------------------------------------------
        let rem: Vec<&WidgetInstance> = if cfg.enabled { cfg.instances_of(WidgetKind::Reminders).collect() } else { vec![] };
        if !rem.is_empty() && !demo {
            let src = reminders.get_or_insert_with(mac_rem::EkReminders::new);
            let shown = rem.iter().any(|w| vis(&presence, w.page, n_pages, false, now).shown);
            let mut access = src.access();
            if let Some(a) = src.poll_answer() {
                rem_requesting = false;
                access = a;
                log::info!("notch-shelf: reminders access {a:?}");
                if let Some(e) = scheds.get_mut(REM_KEY) {
                    e.0.force();
                }
            }
            // Ask lazily: the first time the card is on screen, or on "Allow".
            if access == Access::NotDetermined && !rem_requesting && (rem_ask || (shown && !rem_asked)) {
                rem_asked = true;
                rem_requesting = src.request() == Access::Requesting;
            }
            if rem_requesting && access == Access::NotDetermined {
                access = Access::Requesting;
            }
            let entry = scheds.entry(REM_KEY.into()).or_insert_with(|| (Schedule::new(60.0), 0));
            for id in &rem_done {
                if access == Access::Granted {
                    if let Err(e) = src.complete(id) {
                        log::info!("notch-shelf: completing a reminder: {e}");
                    }
                }
                entry.0.force();
            }
            if all || (shown && !shown_cached) || refresh.iter().any(|r| rem.iter().any(|w| &w.id == r)) {
                entry.0.force();
            }
            let mut fetched = None;
            if access == Access::Granted && entry.0.due(now, &vis(&presence, rem[0].page, n_pages, false, now)) {
                entry.0.start(now);
                let r = src.fetch();
                entry.0.finish(unix_now(), r.is_ok());
                match r {
                    Ok(v) => fetched = Some(v),
                    Err(e) => log::info!("notch-shelf: reminders: {e}"),
                }
            }
            let changed = {
                let d = s.data();
                d.reminders.access != access || fetched.as_ref().is_some_and(|(items, lists)| *items != d.reminders.items || *lists != d.reminders.lists)
            };
            if changed {
                s.set_data(false, |d| {
                    d.reminders.access = access;
                    if let Some((items, lists)) = fetched {
                        d.reminders.items = items;
                        d.reminders.lists = lists;
                    }
                    if access != Access::Granted {
                        d.reminders.items.clear();
                    }
                });
            }
            shown_cached = shown;
        }
        // ----- Sound --------------------------------------------------------------
        let sound_on = cfg.enabled && cfg.widget_enabled(WidgetKind::Sound);
        if sound_on && !demo {
            match s.host.sound.clone() {
                Some(p) => {
                    for c in &sound_cmds {
                        let r = match c {
                            SoundCmd::Device(id) => p.set_default_output(id),
                            SoundCmd::Volume(v) => p.set_volume(*v),
                            SoundCmd::Mute(m) => p.set_muted(*m),
                        };
                        if let Err(e) = r {
                            log::info!("notch-shelf: sound: {e}");
                        }
                    }
                    let shown = cfg.instances_of(WidgetKind::Sound).any(|w| vis(&presence, w.page, n_pages, false, now).shown);
                    let entry = scheds.entry(SOUND_KEY.into()).or_insert_with(|| (Schedule::new(2.0), 0));
                    entry.0.set_interval(if shown { 1.5 } else { 15.0 });
                    if !sound_cmds.is_empty() {
                        entry.0.force();
                    }
                    if entry.0.due(now, &Visibility { shown, ..vis(&presence, usize::MAX, 0, false, now) }) {
                        entry.0.start(now);
                        let st = SoundState::read(p.as_ref());
                        entry.0.finish(unix_now(), true);
                        if s.data().sound != st {
                            s.set_data(false, |d| d.sound = st);
                        }
                    }
                }
                None => {
                    if s.data().sound.available {
                        s.set_data(false, |d| d.sound = SoundState::default());
                    }
                }
            }
        }
        // ----- Shortcuts -------------------------------------------------------------
        for (id, name) in shortcut_runs {
            if demo {
                continue;
            }
            s.set_data(false, |d| {
                let last = d.shortcuts(&id).and_then(|st| st.last.clone());
                d.instances.insert(id.clone(), InstanceData::Shortcuts(ShortcutsState { running: Some(name.clone()), last }));
            });
            let tx = tx.clone();
            let scratch = data_dir.join("notch-run");
            let sig = s.clone();
            let _ = std::thread::Builder::new().name("notch-shortcut".into()).spawn(move || {
                let result = shortcuts::run_shortcut(&name, &scratch, Duration::from_secs(120));
                let _ = tx.send(Done::Shortcut { id, name, result });
                sig.signal.notify();
            });
        }
        // Forget instances that are gone.
        let live_keys: Vec<String> = specs.iter().map(|j| j.key.clone()).chain([BT_KEY.to_string(), REM_KEY.to_string(), SOUND_KEY.to_string()]).collect();
        scheds.retain(|k, _| live_keys.contains(k));
        let ids: Vec<String> = cfg.widgets.iter().filter(|w| w.enabled).map(|w| w.id.clone()).collect();
        if s.data().instances.keys().any(|k| !ids.contains(k)) {
            s.set_data(demo, |d| d.instances.retain(|k, _| ids.contains(k)));
        }
        // Sleep until the next job is due (or a request / result arrives).
        let wake = next_wake(
            scheds.iter().map(|(k, (sc, _))| {
                let (page, live) = specs.iter().find(|j| &j.key == k).map(|j| (j.page, j.live)).unwrap_or((usize::MAX, false));
                (sc, vis(&presence, page, n_pages, live, now))
            }),
            now,
            1.0,
        );
        s.signal.wait(Duration::from_secs_f64(wake));
    }
}

/// Run one Script / xbar / Web job.
fn run_job(id: &str, settings: &KindSettings, cache: Option<WebCacheEntry>) -> (Result<Content, String>, Option<WebCacheEntry>) {
    match settings {
        KindSettings::Script(s) => (run_script(id, s), None),
        KindSettings::Xbar(s) => (run_xbar(s), None),
        KindSettings::Web(s) => match run_web(s, cache) {
            Ok((c, e)) => (Ok(c), Some(e)),
            Err(e) => (Err(e), None),
        },
        _ => (Err("Not a data widget".into()), None),
    }
}

fn widget_env(id: &str) -> Vec<(String, String)> {
    vec![
        ("POWERMAC".into(), "1".into()),
        ("POWERMAC_WIDGET_ID".into(), id.into()),
        // What xbar / SwiftBar plugins look at for the appearance.
        ("XBARDarkMode".into(), "true".into()),
        ("BitBarDarkMode".into(), "1".into()),
        ("OS_APPEARANCE".into(), "Dark".into()),
    ]
}

/// Run a Script widget's command and parse its output.
pub fn run_script(id: &str, s: &ScriptSettings) -> Result<Content, String> {
    if s.command.trim().is_empty() {
        return Err("No command set".into());
    }
    let argv = argv_for(s.interpreter, &s.interpreter_path, &s.command);
    let cwd = xbar::expand_tilde(if s.working_dir.trim().is_empty() { "~" } else { &s.working_dir });
    let out = run::run(&argv, Some(&cwd), &widget_env(id), Duration::from_secs_f64(s.timeout.clamp(1.0, 600.0)))?;
    if !out.ok() {
        return Err(out.error_message());
    }
    let mut c = parse_output(&out.stdout, s.format)?;
    if c.unit.is_empty() && !s.unit.is_empty() && c.number.is_some() {
        c.unit = s.unit.clone();
        if c.unit == "%" && c.progress.is_none() {
            c.progress = c.number.map(|n| (n / 100.0).clamp(0.0, 1.0));
        }
    }
    Ok(c)
}

/// Run an xbar plugin (or every plugin in a folder) and parse the output.
pub fn run_xbar(s: &XbarSettings) -> Result<Content, String> {
    let p = xbar::expand_tilde(&s.path);
    let timeout = Duration::from_secs_f64(s.timeout.clamp(1.0, 600.0));
    let one = |f: &Path| -> Result<Content, String> {
        let cwd = f.parent().map(Path::to_path_buf);
        let out = run::run(&plugin_argv(f), cwd.as_deref(), &widget_env("xbar"), timeout)?;
        if !out.ok() && out.stdout.trim().is_empty() {
            return Err(out.error_message());
        }
        Ok(xbar::to_content(&xbar::parse(&out.stdout)))
    };
    if p.is_dir() {
        let plugins = xbar::list_plugins(&p);
        if plugins.is_empty() {
            return Err(format!("No plugins in {}", s.path.trim()));
        }
        if plugins.len() == 1 {
            return one(&plugins[0]);
        }
        let sections: Vec<(String, Result<Content, String>)> = plugins.iter().take(8).map(|f| (xbar::display_name(&f.file_name().unwrap_or_default().to_string_lossy()), one(f))).collect();
        return Ok(xbar::merge_sections(&sections));
    }
    if !p.exists() {
        return Err(format!("{} doesn't exist", s.path.trim()));
    }
    one(&p)
}

fn web_request_key(s: &WebSettings) -> u64 {
    let h = web::request_headers(s);
    hash_bytes(format!("{}|{:?}", s.url.trim(), h).as_bytes())
}

/// Fetch a Web widget's URL (with an If-None-Match for the cached ETag)
/// and render its templates.
fn run_web(s: &WebSettings, cache: Option<WebCacheEntry>) -> Result<(Content, WebCacheEntry), String> {
    web::check_url(&s.url, s.allow_http)?;
    let key = web_request_key(s);
    let mut headers = web::request_headers(s);
    let cached = cache.filter(|c| c.key == key);
    if let Some(e) = cached.as_ref().and_then(|c| c.etag.clone()) {
        headers.push(("If-None-Match".into(), e));
    }
    let r = rt::http_request(s.url.trim(), &headers, s.timeout.clamp(1.0, 120.0))?;
    let (body, etag) = if r.status == 304 {
        let c = cached.ok_or("HTTP 304 without a cached response")?;
        (c.body, c.etag)
    } else {
        if r.body.len() > 4 << 20 {
            return Err("The response is larger than 4 MB".into());
        }
        (String::from_utf8_lossy(&r.body).into_owned(), r.etag)
    };
    let content = web::to_content(&body, s, rt::utc_offset())?;
    Ok((content, WebCacheEntry { key, body, etag, at: unix_now() }))
}

/// system_profiler (AirPods parts, Magic devices) + IOKit HID batteries.
fn read_bluetooth() -> Result<Vec<BtDevice>, String> {
    let out = run::run(&["/usr/sbin/system_profiler".into(), "SPBluetoothDataType".into(), "-json".into()], None, &[], Duration::from_secs(25))?;
    let prof = if out.ok() { bluetooth::parse_system_profiler(&out.stdout) } else { vec![] };
    let hid = mac_bt::hid_batteries();
    if !out.ok() && hid.is_empty() {
        return Err(out.error_message());
    }
    Ok(bluetooth::merge(prof, &hid, &[]))
}

/// Run a Script / xbar / Web instance once, synchronously (a settings
/// preview). Never asks for permissions.
pub fn run_once(inst: &WidgetInstance) -> Result<Content, String> {
    match KindSettings::parse(inst.kind, &inst.settings) {
        KindSettings::Script(s) => run_script(&inst.id, &s),
        KindSettings::Xbar(s) => run_xbar(&s),
        KindSettings::Web(s) => run_web(&s, None).map(|(c, _)| c),
        _ => Err("Only Script, xbar and Web widgets can be previewed".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ns-jobs-{}-{}", std::process::id(), crate::widgets::instance::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn script_jobs() {
        let s: ScriptSettings = crate::widgets::catalog::typed(WidgetKind::Script, &json!({"command": "echo 42; echo of 100 >&2", "unit": "%", "interpreter": "sh"}));
        let c = run_script("t1", &s).unwrap();
        assert_eq!((c.number, c.unit.as_str(), c.progress), (Some(42.0), "%", Some(0.42)));
        let e: ScriptSettings = crate::widgets::catalog::typed(WidgetKind::Script, &json!({"command": "echo nope >&2; exit 2", "interpreter": "sh"}));
        assert_eq!(run_script("t2", &e).unwrap_err(), "nope");
        let slow: ScriptSettings = crate::widgets::catalog::typed(WidgetKind::Script, &json!({"command": "sleep 5", "interpreter": "sh", "timeout": 1}));
        assert_eq!(run_script("t3", &slow).unwrap_err(), "Timed out");
        let env: ScriptSettings = crate::widgets::catalog::typed(WidgetKind::Script, &json!({"command": "echo $POWERMAC_WIDGET_ID", "interpreter": "sh", "format": "text"}));
        assert_eq!(run_script("abc", &env).unwrap().title, "abc");
        assert!(run_script("x", &ScriptSettings::default()).is_err());
    }

    #[test]
    fn xbar_jobs_and_intervals() {
        let d = scratch();
        let f = d.join("weather.10s.sh");
        std::fs::write(&f, "#!/bin/sh\necho \"21° | color=orange\"\necho ---\necho \"Dark: $XBARDarkMode | href=https://example.com\"\n").unwrap();
        let g = d.join("cpu.2m.sh");
        std::fs::write(&g, "#!/bin/sh\necho CPU 12%\n").unwrap();
        #[cfg(unix)]
        for p in [&f, &g] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let one = XbarSettings { path: f.to_string_lossy().into_owned(), ..Default::default() };
        let c = run_xbar(&one).unwrap();
        assert_eq!(c.title, "21°");
        assert_eq!(c.items[0].title, "Dark: true");
        assert_eq!(xbar_interval(&one), 10.0);
        let folder = XbarSettings { path: d.to_string_lossy().into_owned(), ..Default::default() };
        let c = run_xbar(&folder).unwrap();
        assert_eq!(c.items.iter().filter(|i| i.header).count(), 2);
        assert_eq!(xbar_interval(&folder), 10.0);
        assert_eq!(xbar_interval(&XbarSettings { interval: 7.0, ..folder.clone() }), 7.0);
        assert!(run_xbar(&XbarSettings { path: d.join("missing.sh").to_string_lossy().into_owned(), ..Default::default() }).is_err());
        let empty = scratch();
        assert!(run_xbar(&XbarSettings { path: empty.to_string_lossy().into_owned(), ..Default::default() }).unwrap_err().starts_with("No plugins"));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn web_jobs_refuse_bad_urls_without_network() {
        let s = WebSettings { url: "http://example.com".into(), template: "{{x}}".into(), ..Default::default() };
        assert!(run_web(&s, None).unwrap_err().contains("https"));
        let inst = WidgetInstance::new(WidgetKind::Battery);
        assert!(run_once(&inst).is_err());
        // The request key changes with the token.
        let a = WebSettings { url: "https://a.b".into(), ..Default::default() };
        let b = WebSettings { token: "t".into(), ..a.clone() };
        assert_ne!(web_request_key(&a), web_request_key(&b));
    }

    /// One real request to a public endpoint; run explicitly with
    /// `cargo test -p notch-shelf -- --ignored web_live`.
    #[test]
    #[ignore]
    fn web_live() {
        let s = WebSettings { url: "https://api.github.com/zen".into(), template: "{{text}}".into(), ..Default::default() };
        let (c, e) = run_web(&s, None).unwrap();
        println!("zen: {:?} etag {:?}", c.value, e.etag);
        assert!(c.value.unwrap().len() > 3);
    }
}
