//! World clocks: IANA time zones (via the system zoneinfo database),
//! digital / analog faces and a day / night indicator.

use super::catalog::{ClockEntry, HourFormat};

/// Is `name` a time zone the system knows ("Europe/Paris", "UTC")?
pub fn zone_valid(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && jiff::tz::TimeZone::get(n).is_ok()
}

/// Seconds east of UTC in `zone` at unix time `t`.
pub fn offset_at(zone: &str, t: f64) -> Option<f64> {
    let tz = jiff::tz::TimeZone::get(zone.trim()).ok()?;
    let ts = jiff::Timestamp::from_second(t.floor() as i64).ok()?;
    Some(tz.to_offset(ts).seconds() as f64)
}

/// Label for a zone without one: "America/New_York" → "New York".
pub fn default_label(zone: &str) -> String {
    zone.rsplit('/').next().unwrap_or(zone).replace('_', " ")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub label: String,
    pub zone: String,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// "9:41" / "21:41" (with ":05" seconds when asked).
    pub time: String,
    /// "AM" / "PM" in 12-hour mode.
    pub ampm: Option<&'static str>,
    /// Offset from local time, seconds.
    pub diff: f64,
    /// The zone's date minus the local date (-1, 0, 1).
    pub day_delta: i64,
    /// 6:00–18:00 in that zone.
    pub is_day: bool,
}

/// "+9h", "−3h", "+5:30", "Same time".
pub fn diff_label(diff: f64) -> String {
    let mins = (diff / 60.0).round() as i64;
    if mins == 0 {
        return "Same time".into();
    }
    let sign = if mins > 0 { "+" } else { "\u{2212}" };
    let a = mins.abs();
    if a % 60 == 0 {
        format!("{sign}{}h", a / 60)
    } else {
        format!("{sign}{}:{:02}", a / 60, a % 60)
    }
}

/// "Today", "Tomorrow", "Yesterday" relative to local time, combined
/// with the offset ("Tomorrow, +9h").
pub fn caption(f: &Face) -> String {
    let day = match f.day_delta {
        0 => "Today",
        d if d > 0 => "Tomorrow",
        _ => "Yesterday",
    };
    format!("{day}, {}", diff_label(f.diff))
}

/// The face of `entry` at `unix_now` (local offset `local_offset`).
pub fn face(entry: &ClockEntry, unix_now: f64, local_offset: f64, h24: bool, seconds: bool) -> Option<Face> {
    let off = offset_at(&entry.zone, unix_now)?;
    let local = unix_now + off;
    let secs = local.floor() as i64;
    let tod = secs.rem_euclid(86_400);
    let (hour, minute, second) = ((tod / 3600) as u32, ((tod / 60) % 60) as u32, (tod % 60) as u32);
    let day_delta = (local / 86_400.0).floor() as i64 - ((unix_now + local_offset) / 86_400.0).floor() as i64;
    let (h, ampm) = if h24 {
        (hour, None)
    } else {
        let h = match hour % 12 {
            0 => 12,
            h => h,
        };
        (h, Some(if hour < 12 { "AM" } else { "PM" }))
    };
    let time = if seconds { format!("{h}:{minute:02}:{second:02}") } else { format!("{h}:{minute:02}") };
    let label = if entry.label.trim().is_empty() { default_label(&entry.zone) } else { entry.label.trim().to_string() };
    Some(Face { label, zone: entry.zone.clone(), hour, minute, second, time, ampm, diff: off - local_offset, day_delta, is_day: (6..18).contains(&hour) })
}

pub fn use_24h(f: HourFormat, system_24h: bool) -> bool {
    match f {
        HourFormat::Auto => system_24h,
        HourFormat::H12 => false,
        HourFormat::H24 => true,
    }
}

/// Hand angles in radians, clockwise from 12 o'clock: (hour, minute,
/// second). The hour and minute hands move continuously.
pub fn hand_angles(hour: u32, minute: u32, second: f64) -> (f64, f64, f64) {
    use std::f64::consts::TAU;
    let s = second / 60.0;
    let m = (minute as f64 + s) / 60.0;
    let h = ((hour % 12) as f64 + m) / 12.0;
    (h * TAU, m * TAU, s * TAU)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(zone: &str, label: &str) -> ClockEntry {
        ClockEntry { zone: zone.into(), label: label.into() }
    }

    #[test]
    fn zones_and_offsets() {
        assert!(zone_valid("Europe/Paris") && zone_valid("UTC") && zone_valid("Asia/Kolkata"));
        assert!(!zone_valid("Mars/Olympus") && !zone_valid("") && !zone_valid("../../etc/passwd"));
        // 2026-01-15 12:00 UTC: Paris is +1, New York -5, Kolkata +5:30.
        let jan = 1_768_478_400.0;
        assert_eq!(offset_at("Europe/Paris", jan), Some(3600.0));
        assert_eq!(offset_at("America/New_York", jan), Some(-5.0 * 3600.0));
        assert_eq!(offset_at("Asia/Kolkata", jan), Some(5.5 * 3600.0));
        // 2026-07-15: daylight saving time.
        let jul = 1_784_116_800.0;
        assert_eq!(offset_at("Europe/Paris", jul), Some(7200.0));
        assert_eq!(offset_at("America/New_York", jul), Some(-4.0 * 3600.0));
        // Southern hemisphere: Sydney is +11 in January, +10 in July.
        assert_eq!(offset_at("Australia/Sydney", jan), Some(11.0 * 3600.0));
        assert_eq!(offset_at("Australia/Sydney", jul), Some(10.0 * 3600.0));
        assert_eq!(offset_at("Nowhere/City", jan), None);
        assert_eq!(default_label("America/Los_Angeles"), "Los Angeles");
    }

    #[test]
    fn faces() {
        // 2026-01-15 23:30 UTC, local = Berlin (+1 → 00:30 next day).
        let t = 1_768_519_800.0;
        let local = 3600.0;
        let tokyo = face(&e("Asia/Tokyo", ""), t, local, true, false).unwrap();
        assert_eq!((tokyo.label.as_str(), tokyo.time.as_str(), tokyo.hour, tokyo.minute), ("Tokyo", "8:30", 8, 30));
        assert_eq!(tokyo.diff, 8.0 * 3600.0);
        assert_eq!(tokyo.day_delta, 0);
        assert!(tokyo.is_day);
        assert_eq!(caption(&tokyo), "Today, +8h");
        let la = face(&e("America/Los_Angeles", "Cupertino"), t, local, false, true).unwrap();
        assert_eq!((la.label.as_str(), la.time.as_str(), la.ampm), ("Cupertino", "3:30:00", Some("PM")));
        assert_eq!(la.day_delta, -1);
        assert!(la.is_day);
        assert_eq!(caption(&la), "Yesterday, \u{2212}9h");
        let utc = face(&e("UTC", "UTC"), t, local, false, false).unwrap();
        assert_eq!((utc.time.as_str(), utc.ampm, utc.is_day), ("11:30", Some("PM"), false));
        let delhi = face(&e("Asia/Kolkata", "Delhi"), t, local, true, false).unwrap();
        assert_eq!(diff_label(delhi.diff), "+4:30");
        assert_eq!(diff_label(0.0), "Same time");
        assert_eq!(diff_label(-34_200.0), "\u{2212}9:30");
        // Midnight / noon in 12-hour mode.
        let mid = face(&e("UTC", ""), 1_768_435_200.0, 0.0, false, false).unwrap();
        assert_eq!((mid.time.as_str(), mid.ampm), ("12:00", Some("AM")));
        assert!(face(&e("Bad/Zone", ""), t, 0.0, true, false).is_none());
        assert!(use_24h(HourFormat::Auto, true) && !use_24h(HourFormat::H12, true) && use_24h(HourFormat::H24, false));
    }

    #[test]
    fn hands() {
        use std::f64::consts::{PI, TAU};
        let (h, m, s) = hand_angles(3, 0, 0.0);
        assert!((h - PI / 2.0).abs() < 1e-9 && m == 0.0 && s == 0.0);
        let (h, m, s) = hand_angles(18, 30, 15.0);
        assert!((h - (6.5 + 0.25 / 60.0) / 12.0 * TAU).abs() < 1e-9);
        assert!((m - (30.25 / 60.0) * TAU).abs() < 1e-9);
        assert!((s - PI / 2.0).abs() < 1e-9);
    }
}
