//! Fake widget data for previews and offscreen renders (no permissions,
//! no network).

use std::sync::Arc;

use super::stats::{Pressure, HISTORY};
use super::{Access, BatteryInfo, CalEvent, MediaSource, NowPlaying, Rgba, WeatherNow, WeatherStatus, WidgetData};

pub fn data(unix_now: f64, utc_offset: f64, artwork: Option<Vec<u8>>) -> WidgetData {
    let mut d = WidgetData {
        now_playing: Some(NowPlaying {
            title: "Midnight City".into(),
            artist: "M83".into(),
            album: "Hurry Up, We're Dreaming".into(),
            app: "Music".into(),
            playing: true,
            duration: Some(243.0),
            elapsed: Some(97.0),
            sampled_at: unix_now,
            artwork: artwork.map(Arc::new),
            source: MediaSource::Music,
        }),
        calendar_access: Access::Granted,
        events: vec![],
        weather: WeatherStatus::Ready(WeatherNow { place: "Cupertino".into(), temp: 21.4, high: 24.0, low: 13.0, code: 2, is_day: true, fahrenheit: false, fetched_at: unix_now }),
        battery: Some(BatteryInfo { percent: 82, on_ac: true, charging: true, minutes_to_empty: None, minutes_to_full: Some(38) }),
        stats: Default::default(),
        camera_access: Access::NotDetermined,
    };
    // Events relative to the next round half hour.
    let base = (unix_now / 1800.0).floor() * 1800.0;
    let blue = Rgba(0.04, 0.52, 1.0, 1.0);
    let purple = Rgba(0.69, 0.32, 0.87, 1.0);
    let orange = Rgba(1.0, 0.58, 0.0, 1.0);
    let label = |ts: f64| {
        let m = (((ts + utc_offset) / 60.0).floor() as i64).rem_euclid(1440);
        format!("{}:{:02}", m / 60, m % 60)
    };
    for (title, start, mins, color) in [
        ("Design review", base - 600.0, 45.0, purple),
        ("Lunch with Sam", base + 1800.0 * 3.0, 60.0, orange),
        ("Ship notch widgets", base + 1800.0 * 6.0, 30.0, blue),
    ] {
        d.events.push(CalEvent {
            id: String::new(),
            title: title.into(),
            start,
            end: start + mins * 60.0,
            all_day: false,
            color,
            calendar: "Work".into(),
            location: String::new(),
            time_label: label(start),
        });
    }
    for i in 0..HISTORY {
        let x = i as f64;
        d.stats.cpu.push(0.18 + 0.12 * (x * 0.45).sin().abs() + if i > 30 { 0.25 } else { 0.0 });
        d.stats.mem.push(0.61 + 0.02 * (x * 0.2).sin());
        d.stats.rx.push(1.2e6 * (0.5 + 0.5 * (x * 0.3).sin()).powi(2));
        d.stats.tx.push(2.5e5 * (0.5 + 0.5 * (x * 0.21 + 1.0).cos()));
    }
    d.stats.pressure = Pressure::Normal;
    d
}

#[cfg(test)]
mod tests {
    #[test]
    fn demo_data_is_complete() {
        let d = super::data(1_792_972_800.0 + 36_000.0, 0.0, None);
        assert!(d.now_playing.unwrap().playing);
        assert_eq!(d.events.len(), 3);
        assert_eq!(d.events[1].time_label, "11:30");
        assert_eq!(d.stats.cpu.values.len(), super::HISTORY);
    }
}
