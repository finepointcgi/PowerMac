//! Calendar widget logic: which events to show and how to label them.

use super::CalEvent;

/// Local day number of a unix time, given the UTC offset in seconds.
pub fn day_index(ts: f64, utc_offset: f64) -> i64 {
    ((ts + utc_offset) / 86_400.0).floor() as i64
}

/// Start of the local day containing `ts`.
pub fn day_start(ts: f64, utc_offset: f64) -> f64 {
    day_index(ts, utc_offset) as f64 * 86_400.0 - utc_offset
}

/// Events still worth showing at `now`: not ended, in the next `days`
/// local days, timed events first by start, all-day ones after the timed
/// ones of the same day. Calendar filter: titles in `only` (empty = all).
pub fn upcoming(events: &[CalEvent], now: f64, utc_offset: f64, days: u32, only: &[String]) -> Vec<CalEvent> {
    let today = day_index(now, utc_offset);
    let last = today + days.max(1) as i64 - 1;
    let mut v: Vec<CalEvent> = events
        .iter()
        .filter(|e| e.end > now)
        .filter(|e| only.is_empty() || only.iter().any(|c| c.eq_ignore_ascii_case(&e.calendar)))
        .filter(|e| {
            let d = day_index(e.start.max(now), utc_offset);
            d >= today && d <= last
        })
        .cloned()
        .collect();
    v.sort_by(|a, b| {
        let da = day_index(a.start.max(now), utc_offset);
        let db = day_index(b.start.max(now), utc_offset);
        da.cmp(&db).then(a.all_day.cmp(&b.all_day)).then(a.start.total_cmp(&b.start)).then(a.title.cmp(&b.title))
    });
    v
}

/// Right-hand label: "now", "in 5 min", "in 2 h", or the start time, with
/// "Tomorrow" / weekday prefixes for later days.
pub fn when_label(e: &CalEvent, now: f64, utc_offset: f64) -> String {
    let today = day_index(now, utc_offset);
    let day = day_index(e.start.max(now), utc_offset);
    if e.all_day {
        return match day - today {
            0 => "All day".into(),
            1 => "Tomorrow".into(),
            _ => weekday(e.start, utc_offset).into(),
        };
    }
    if e.start <= now {
        return "Now".into();
    }
    let mins = ((e.start - now) / 60.0).ceil() as i64;
    if day == today && mins <= 60 {
        return format!("in {mins} min");
    }
    match day - today {
        0 => e.time_label.clone(),
        1 => format!("Tmrw {}", e.time_label),
        _ => format!("{} {}", weekday(e.start, utc_offset), e.time_label),
    }
}

fn weekday(ts: f64, utc_offset: f64) -> &'static str {
    // 1970-01-01 was a Thursday.
    const D: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    D[day_index(ts, utc_offset).rem_euclid(7) as usize]
}

/// Local (year, month 1-12, day 1-31) of a unix time.
pub fn civil_date(ts: f64, utc_offset: f64) -> (i64, u32, u32) {
    // Howard Hinnant's days → civil algorithm.
    let z = day_index(ts, utc_offset) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

/// "Mon, Oct 26".
pub fn date_header(ts: f64, utc_offset: f64) -> String {
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (_, m, d) = civil_date(ts, utc_offset);
    format!("{}, {} {d}", weekday(ts, utc_offset), M[(m as usize - 1).min(11)])
}

/// Is the event happening right now (for an accent highlight)?
pub fn is_now(e: &CalEvent, now: f64) -> bool {
    !e.all_day && e.start <= now && now < e.end
}

/// `ical://` URL that opens an event in Calendar.app.
pub fn open_url(e: &CalEvent) -> Option<String> {
    if e.id.is_empty() {
        return None;
    }
    let mut id = String::new();
    for b in e.id.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => id.push(b as char),
            _ => id.push_str(&format!("%{b:02X}")),
        }
    }
    Some(format!("ical://ekevent/{id}?method=show&options=more"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-10-26 00:00 UTC is a Monday.
    const MON: f64 = 1_792_972_800.0;

    fn ev(title: &str, start: f64, mins: f64, all_day: bool) -> CalEvent {
        CalEvent { id: title.into(), title: title.into(), start, end: start + mins * 60.0, all_day, calendar: "Work".into(), time_label: "T".into(), ..Default::default() }
    }

    #[test]
    fn days_and_offsets() {
        assert_eq!(weekday(MON + 3600.0, 0.0), "Mon");
        // 23:30 UTC Monday is Tuesday in UTC+2.
        assert_eq!(day_index(MON + 23.5 * 3600.0, 7200.0), day_index(MON, 0.0) + 1);
        assert_eq!(day_start(MON + 5000.0, 0.0), MON);
        assert_eq!(day_start(MON + 5000.0, -3600.0), MON + 3600.0);
    }

    #[test]
    fn upcoming_filters_and_sorts() {
        let now = MON + 10.0 * 3600.0; // 10:00
        let events = vec![
            ev("ended", MON + 8.0 * 3600.0, 60.0, false),
            ev("lunch", MON + 12.0 * 3600.0, 60.0, false),
            ev("standup", MON + 9.5 * 3600.0, 60.0, false), // running until 10:30
            ev("holiday", MON, 24.0 * 60.0, true),
            ev("tomorrow", MON + 86_400.0 + 9.0 * 3600.0, 30.0, false),
            ev("later", MON + 3.0 * 86_400.0, 30.0, false),
        ];
        let v = upcoming(&events, now, 0.0, 2, &[]);
        let names: Vec<&str> = v.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(names, ["standup", "lunch", "holiday", "tomorrow"]);
        assert_eq!(upcoming(&events, now, 0.0, 1, &[]).len(), 3);
        assert!(upcoming(&events, now, 0.0, 2, &["Home".into()]).is_empty());
        assert_eq!(upcoming(&events, now, 0.0, 2, &["work".into()]).len(), 4);
    }

    #[test]
    fn labels() {
        let now = MON + 10.0 * 3600.0;
        assert_eq!(when_label(&ev("a", now - 60.0, 30.0, false), now, 0.0), "Now");
        assert_eq!(when_label(&ev("a", now + 290.0, 30.0, false), now, 0.0), "in 5 min");
        assert_eq!(when_label(&ev("a", now + 3.0 * 3600.0, 30.0, false), now, 0.0), "T");
        assert_eq!(when_label(&ev("a", now + 86_400.0, 30.0, false), now, 0.0), "Tmrw T");
        assert_eq!(when_label(&ev("a", MON + 2.0 * 86_400.0 + 3600.0, 30.0, false), now, 0.0), "Wed T");
        assert_eq!(when_label(&ev("a", MON, 1440.0, true), now, 0.0), "All day");
        assert_eq!(when_label(&ev("a", MON + 86_400.0, 1440.0, true), now, 0.0), "Tomorrow");
        assert!(is_now(&ev("a", now - 1.0, 5.0, false), now));
        assert!(!is_now(&ev("a", MON, 1440.0, true), now));
    }

    #[test]
    fn dates() {
        assert_eq!(civil_date(MON, 0.0), (2026, 10, 26));
        assert_eq!(civil_date(0.0, 0.0), (1970, 1, 1));
        assert_eq!(civil_date(951_782_400.0, 0.0), (2000, 2, 29));
        assert_eq!(date_header(MON + 3600.0, 0.0), "Mon, Oct 26");
        assert_eq!(date_header(MON - 3600.0, 0.0), "Sun, Oct 25");
    }

    #[test]
    fn open_urls() {
        let mut e = ev("x", 0.0, 1.0, false);
        e.id = "ABC-1:2/3".into();
        assert_eq!(open_url(&e).unwrap(), "ical://ekevent/ABC-1%3A2%2F3?method=show&options=more");
        e.id.clear();
        assert!(open_url(&e).is_none());
    }
}
