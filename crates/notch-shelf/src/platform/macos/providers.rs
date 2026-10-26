//! Background threads that feed [`crate::widgets::WidgetData`]: system
//! stats + battery, now playing, calendar, weather. Each idles (a wakeup
//! every second or two) while its widget is off, and exits when the shelf
//! is stopped.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use objc2::rc::autoreleasepool;
use serde::{Deserialize, Serialize};

use super::{calendar, media, rt, sysinfo};
use crate::widgets::stats::{self, Pressure};
use crate::widgets::weather::{self, Place};
use crate::widgets::{unix_now, Access, MediaSource, NowPlaying, TempUnit, WeatherNow, WeatherStatus, WidgetKind};
use crate::{Config, Shared};

pub fn spawn(shared: Arc<Shared>) {
    let run = |name: &str, f: fn(Arc<Shared>), s: Arc<Shared>| {
        let _ = std::thread::Builder::new().name(name.into()).spawn(move || f(s));
    };
    run("notch-stats", stats_loop, shared.clone());
    run("notch-media", media_loop, shared.clone());
    run("notch-calendar", calendar_loop, shared.clone());
    run("notch-weather", weather_loop, shared.clone());
    super::jobs::spawn(shared);
}

/// "Read Music and Spotify directly" is on in any Now Playing widget.
fn use_applescript(cfg: &Config) -> bool {
    cfg.instances_of(WidgetKind::NowPlaying).any(|w| crate::widgets::catalog::typed::<crate::widgets::catalog::NowPlayingSettings>(w.kind, &w.settings).use_applescript)
}

/// Days the calendar widgets look ahead (the most any of them wants).
fn calendar_days(cfg: &Config) -> u32 {
    cfg.instances_of(WidgetKind::Calendar).map(|w| crate::widgets::catalog::typed::<crate::widgets::catalog::CalendarSettings>(w.kind, &w.settings).days.clamp(1.0, 7.0) as u32).max().unwrap_or(2)
}

fn config(s: &Shared) -> Option<Config> {
    let st = s.lock();
    (!st.stopped).then(|| st.config.clone())
}

fn on(cfg: &Config, k: WidgetKind) -> bool {
    cfg.enabled && cfg.widget_enabled(k)
}

fn demo(s: &Shared) -> bool {
    s.demo.load(Ordering::Relaxed)
}

// ----- stats + battery ---------------------------------------------------

fn stats_loop(s: Arc<Shared>) {
    let mut prev_cpu = None;
    let mut prev_net: Option<(f64, Vec<(String, stats::NetCounters)>)> = None;
    let mut last_batt = 0.0;
    while let Some(cfg) = config(&s) {
        if demo(&s) {
            s.signal.wait(Duration::from_secs(1));
            continue;
        }
        let now = unix_now();
        if on(&cfg, WidgetKind::SystemStats) {
            let cpu = sysinfo::cpu_ticks();
            let load = match (&prev_cpu, &cpu) {
                (Some(a), Some(b)) => stats::cpu_load(a, b),
                _ => None,
            };
            prev_cpu = cpu;
            let mem = sysinfo::memory();
            let net = sysinfo::net_counters();
            let rate = prev_net.as_ref().map(|(t, p)| stats::net_rate(p, &net, now - t, 1 << 32));
            prev_net = Some((now, net));
            s.set_data(false, |d| {
                if let Some(l) = load {
                    d.stats.cpu.push(l);
                }
                if let Some((m, lvl)) = mem {
                    d.stats.mem.push(m);
                    d.stats.pressure = Pressure::from_level(lvl);
                }
                if let Some((rx, tx)) = rate {
                    d.stats.rx.push(rx);
                    d.stats.tx.push(tx);
                }
            });
        } else if prev_cpu.is_some() {
            prev_cpu = None;
            prev_net = None;
            s.set_data(false, |d| d.stats = Default::default());
        }
        let batt_every = if s.home_visible.load(Ordering::Relaxed) { 2.0 } else { 20.0 };
        if on(&cfg, WidgetKind::Battery) && now - last_batt >= batt_every {
            last_batt = now;
            let b = sysinfo::battery();
            if s.data().battery != b {
                s.set_data(false, |d| d.battery = b);
            }
        }
        s.signal.wait(Duration::from_secs(1));
    }
}

// ----- now playing -------------------------------------------------------

fn media_loop(s: Arc<Shared>) {
    let mut last_poll = 0.0;
    let mut art_for: Option<NowPlaying> = None;
    let mut art: Option<Arc<Vec<u8>>> = None;
    while let Some(cfg) = config(&s) {
        let cmds = std::mem::take(&mut s.reqs().media);
        if demo(&s) {
            s.signal.wait(Duration::from_secs(1));
            continue;
        }
        if !on(&cfg, WidgetKind::NowPlaying) {
            if s.data().now_playing.is_some() {
                s.set_data(false, |d| d.now_playing = None);
            }
            s.signal.wait(Duration::from_secs(2));
            continue;
        }
        let source = s.data().now_playing.as_ref().map(|n| n.source).unwrap_or_default();
        let sent = !cmds.is_empty();
        for c in cmds {
            media::send(c, source);
        }
        if sent {
            // Let the player react before re-reading.
            std::thread::sleep(Duration::from_millis(250));
        }
        let visible = s.home_visible.load(Ordering::Relaxed);
        let every = if visible { 1.0 } else if cfg.live_activities { 2.5 } else { 6.0 };
        let now = unix_now();
        if sent || now - last_poll >= every {
            last_poll = now;
            let np = autoreleasepool(|_| sample_media(&cfg, now, &mut art_for, &mut art));
            if s.data().now_playing != Some(np.clone()) {
                s.set_data(false, |d| d.now_playing = Some(np));
            }
        }
        s.signal.wait(Duration::from_millis(500));
    }
}

fn sample_media(cfg: &Config, now: f64, art_for: &mut Option<NowPlaying>, art: &mut Option<Arc<Vec<u8>>>) -> NowPlaying {
    if let Some(np) = media::query_media_remote() {
        return np;
    }
    if let Some(np) = media::query_jxa(now) {
        return np;
    }
    if use_applescript(cfg) {
        if let Some((mut np, url)) = media::query_applescript(now) {
            // Fetch artwork once per track.
            let same = art_for.as_ref().is_some_and(|a| crate::widgets::media::same_track(a, &np));
            if !same {
                *art = media::applescript_artwork(&np, url.as_deref()).map(Arc::new);
                *art_for = Some(np.clone());
            }
            np.artwork = art.clone();
            return np;
        }
    }
    NowPlaying { source: MediaSource::MediaKeys, sampled_at: now, ..Default::default() }
}

// ----- calendar ----------------------------------------------------------

fn calendar_loop(s: Arc<Shared>) {
    let mut cal: Option<calendar::Calendar> = None;
    let mut last_fetch = f64::NEG_INFINITY;
    let mut was_visible = false;
    let mut asked = false;
    let mut requesting = false;
    while let Some(cfg) = config(&s) {
        if demo(&s) || !on(&cfg, WidgetKind::Calendar) {
            s.signal.wait(Duration::from_secs(2));
            continue;
        }
        let cal = cal.get_or_insert_with(calendar::Calendar::new);
        let now = unix_now();
        let visible = s.home_visible.load(Ordering::Relaxed);
        let (ask, refresh) = {
            let mut r = s.reqs();
            (std::mem::take(&mut r.calendar_ask), std::mem::take(&mut r.refresh_calendar))
        };
        let mut access = autoreleasepool(|_| calendar::access());
        if let Some(a) = cal.poll_answer() {
            requesting = false;
            access = a;
            log::info!("notch-shelf: calendar access {a:?}");
        }
        // Ask lazily: the first time the card is on screen, or on "Allow".
        if access == Access::NotDetermined && !requesting && (ask || (visible && !asked)) {
            asked = true;
            requesting = autoreleasepool(|_| cal.request()) == Access::Requesting;
        }
        if requesting && access == Access::NotDetermined {
            access = Access::Requesting;
        }
        let mut events = None;
        if access == Access::Granted {
            let due = refresh || now - last_fetch > 300.0 || (visible && !was_visible && now - last_fetch > 30.0);
            if due {
                last_fetch = now;
                let off = rt::utc_offset();
                let from = crate::widgets::calendar::day_start(now, off);
                let to = from + 86_400.0 * calendar_days(&cfg) as f64;
                events = Some(cal.events(from, to));
            }
        }
        was_visible = visible;
        let changed = {
            let d = s.data();
            d.calendar_access != access || events.as_ref().is_some_and(|e| *e != d.events)
        };
        if changed {
            s.set_data(false, |d| {
                d.calendar_access = access;
                if let Some(e) = events {
                    d.events = e;
                }
                if access != Access::Granted {
                    d.events.clear();
                }
            });
        }
        s.signal.wait(Duration::from_secs(1));
    }
}

// ----- weather -----------------------------------------------------------

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct WeatherCache {
    query: String,
    fahrenheit: bool,
    place: Option<Place>,
    now: Option<WeatherNow>,
}

const WEATHER_FILE: &str = "notch-weather.json";

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct WeatherFile {
    entries: Vec<WeatherCache>,
}

/// One location's forecast state.
struct WeatherSlot {
    cache: WeatherCache,
    last_attempt: f64,
    last_error: Option<String>,
}

fn load_weather(path: &std::path::Path) -> Vec<WeatherCache> {
    let Ok(b) = std::fs::read(path) else { return vec![] };
    if let Ok(f) = serde_json::from_slice::<WeatherFile>(&b) {
        if !f.entries.is_empty() {
            return f.entries;
        }
    }
    // The single-location file of older versions.
    serde_json::from_slice::<WeatherCache>(&b).ok().filter(|c| !c.query.is_empty()).into_iter().collect()
}

fn weather_loop(s: Arc<Shared>) {
    let path: PathBuf = s.lock().store.data_dir().join(WEATHER_FILE);
    let mut slots: Vec<WeatherSlot> = load_weather(&path).into_iter().map(|cache| WeatherSlot { cache, last_attempt: f64::NEG_INFINITY, last_error: None }).collect();
    while let Some(cfg) = config(&s) {
        let refresh = std::mem::take(&mut s.reqs().refresh_weather);
        if demo(&s) {
            s.signal.wait(Duration::from_secs(2));
            continue;
        }
        let instances: Vec<(String, crate::widgets::catalog::WeatherSettings)> =
            if cfg.enabled { cfg.instances_of(WidgetKind::Weather).map(|w| (w.id.clone(), crate::widgets::catalog::typed(w.kind, &w.settings))).collect() } else { vec![] };
        let publish = |statuses: Vec<(String, WeatherStatus)>| {
            let first = statuses.first().map(|(_, st)| st.clone()).unwrap_or(WeatherStatus::Off);
            let changed = {
                let d = s.data();
                d.weather != first || statuses.len() != d.weathers.len() || statuses.iter().any(|(id, st)| d.weathers.get(id) != Some(st))
            };
            if changed {
                s.set_data(false, |d| {
                    d.weather = first;
                    d.weathers = statuses.into_iter().collect();
                });
            }
        };
        if instances.is_empty() {
            publish(vec![]);
            s.signal.wait(Duration::from_secs(2));
            continue;
        }
        let mut statuses = vec![];
        let mut wrote = false;
        for (id, ws) in &instances {
            let query = ws.location.trim().to_string();
            if query.is_empty() {
                statuses.push((id.clone(), WeatherStatus::NoLocation));
                continue;
            }
            let fahrenheit = match ws.units {
                TempUnit::Fahrenheit => true,
                TempUnit::Celsius => false,
                TempUnit::Auto => autoreleasepool(|_| rt::prefers_fahrenheit()),
            };
            let k = match slots.iter().position(|sl| sl.cache.query == query && sl.cache.fahrenheit == fahrenheit) {
                Some(k) => k,
                None => {
                    // The same place in other units shares the geocoding.
                    let place = slots.iter().find(|sl| sl.cache.query == query).and_then(|sl| sl.cache.place.clone());
                    slots.push(WeatherSlot { cache: WeatherCache { query: query.clone(), fahrenheit, place, now: None }, last_attempt: f64::NEG_INFINITY, last_error: None });
                    slots.len() - 1
                }
            };
            let sl = &mut slots[k];
            let now = unix_now();
            let fresh = sl.cache.now.as_ref().is_some_and(|w| weather::is_fresh(w.fetched_at, now));
            let age = sl.cache.now.as_ref().map(|w| now - w.fetched_at).unwrap_or(f64::INFINITY);
            let retry_ok = now - sl.last_attempt >= if sl.last_error.is_some() { weather::RETRY_SECS } else { 0.0 };
            if (!fresh && retry_ok) || (refresh && age > 300.0) {
                sl.last_attempt = now;
                match fetch_weather(&query, fahrenheit, &mut sl.cache, now) {
                    Ok(()) => {
                        sl.last_error = None;
                        wrote = true;
                    }
                    Err(e) => {
                        log::info!("notch-shelf: weather: {e}");
                        sl.last_error = Some(e);
                    }
                }
            }
            let st = match (&sl.last_error, &sl.cache.now) {
                (None, Some(w)) => WeatherStatus::Ready(w.clone()),
                (Some(e), w) => WeatherStatus::Error(e.clone(), w.clone()),
                (None, None) => WeatherStatus::Loading,
            };
            statuses.push((id.clone(), st));
        }
        // Forget places no widget shows any more.
        let wanted: Vec<String> = instances.iter().map(|(_, w)| w.location.trim().to_string()).collect();
        slots.retain(|sl| wanted.contains(&sl.cache.query));
        if wrote {
            let f = WeatherFile { entries: slots.iter().map(|sl| sl.cache.clone()).collect() };
            if let Ok(b) = serde_json::to_vec_pretty(&f) {
                let _ = std::fs::write(&path, b);
            }
        }
        publish(statuses);
        s.signal.wait(Duration::from_secs(5));
    }
}

fn fetch_weather(query: &str, fahrenheit: bool, cache: &mut WeatherCache, now: f64) -> Result<(), String> {
    let place = match &cache.place {
        Some(p) => p.clone(),
        None => {
            let p = match weather::parse_coords(query) {
                Some((lat, lon)) => Place { name: format!("{:.1}°, {:.1}°", lat, lon), lat, lon },
                None => {
                    let body = rt::http_get(&weather::geocoding_url(query), 12.0).map_err(|e| format!("Couldn't reach Open-Meteo ({e})"))?;
                    weather::parse_geocoding(&String::from_utf8_lossy(&body), query)?
                }
            };
            cache.place = Some(p.clone());
            p
        }
    };
    let body = rt::http_get(&weather::forecast_url(place.lat, place.lon, fahrenheit), 12.0).map_err(|e| format!("Couldn't reach Open-Meteo ({e})"))?;
    cache.now = Some(weather::parse_forecast(&String::from_utf8_lossy(&body), &place.name, fahrenheit, now)?);
    Ok(())
}

/// One live call to Open-Meteo (geocoding + forecast); run explicitly with
/// `cargo test -p notch-shelf -- --ignored open_meteo_live`.
#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn open_meteo_live() {
        let mut c = super::WeatherCache::default();
        super::fetch_weather("Berlin", false, &mut c, crate::widgets::unix_now()).unwrap();
        let w = c.now.unwrap();
        println!("{} {:.1}° code {} H {:.1} L {:.1} day {}", w.place, w.temp, w.code, w.high, w.low, w.is_day);
        assert_eq!(w.place, "Berlin");
        assert!(w.temp > -60.0 && w.temp < 60.0 && w.high >= w.low);
    }
}
