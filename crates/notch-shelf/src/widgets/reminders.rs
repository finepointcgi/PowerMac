//! Reminders widget logic: which incomplete reminders to show, in what
//! order, and their due labels. The EventKit access is in the platform
//! layer.

use super::calendar::day_index;
use super::catalog::{ReminderFilter, RemindersSettings};
use super::{Access, Rgba};

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Reminder {
    /// EventKit `calendarItemIdentifier`.
    pub id: String,
    pub title: String,
    pub list: String,
    pub color: Rgba,
    /// Unix seconds.
    pub due: Option<f64>,
    /// The due date has no time.
    pub due_all_day: bool,
    /// EventKit priority: 0 none, 1 high … 9 low.
    pub priority: u8,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RemindersState {
    pub access: Access,
    pub items: Vec<Reminder>,
    /// All list names (for the settings UI).
    pub lists: Vec<String>,
}

/// Shown reminders: list filter, the "show" filter, then overdue / due
/// soonest first, undated ones after (high priority first), by title.
pub fn visible<'a>(items: &'a [Reminder], s: &RemindersSettings, now: f64, utc_offset: f64, hide: &[String]) -> Vec<&'a Reminder> {
    let today = day_index(now, utc_offset);
    let mut v: Vec<&Reminder> = items
        .iter()
        .filter(|r| !hide.contains(&r.id))
        .filter(|r| s.lists.is_empty() || s.lists.iter().any(|l| l.trim().eq_ignore_ascii_case(r.list.trim())))
        .filter(|r| match s.show {
            ReminderFilter::All => true,
            ReminderFilter::Scheduled => r.due.is_some(),
            ReminderFilter::Today => r.due.is_some_and(|d| day_index(d, utc_offset) <= today),
        })
        .collect();
    let prio = |p: u8| if p == 0 { 10 } else { p };
    v.sort_by(|a, b| match (a.due, b.due) {
        (Some(x), Some(y)) => x.total_cmp(&y).then(prio(a.priority).cmp(&prio(b.priority))),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => prio(a.priority).cmp(&prio(b.priority)).then(a.title.to_lowercase().cmp(&b.title.to_lowercase())),
    });
    v
}

/// Overdue (red) / today / later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Urgency {
    Overdue,
    Today,
    Later,
    None,
}

/// "Overdue", "Today", "14:30", "Tomorrow", "Mon", "May 3" and its urgency.
pub fn due_label(r: &Reminder, now: f64, utc_offset: f64) -> (String, Urgency) {
    let Some(d) = r.due else { return (String::new(), Urgency::None) };
    let today = day_index(now, utc_offset);
    let day = day_index(d, utc_offset);
    let time = || {
        let m = (((d + utc_offset) / 60.0).floor() as i64).rem_euclid(1440);
        format!("{}:{:02}", m / 60, m % 60)
    };
    if day < today || (!r.due_all_day && d < now) {
        let txt = if day < today { "Overdue".to_string() } else { time() };
        return (txt, Urgency::Overdue);
    }
    if day == today {
        return (if r.due_all_day { "Today".into() } else { time() }, Urgency::Today);
    }
    if day == today + 1 {
        return ("Tomorrow".into(), Urgency::Later);
    }
    if day < today + 7 {
        const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
        return (WD[day.rem_euclid(7) as usize].into(), Urgency::Later);
    }
    let (_, m, dd) = super::calendar::civil_date(d, utc_offset);
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    (format!("{} {}", M[(m as usize).saturating_sub(1).min(11)], dd), Urgency::Later)
}

/// A reminders back end (EventKit, or a fake for previews and tests).
pub trait RemindersSource {
    fn access(&self) -> Access;
    /// Ask for access (may show the system prompt).
    fn request(&mut self) -> Access;
    fn poll_answer(&mut self) -> Option<Access> {
        None
    }
    fn fetch(&mut self) -> Result<(Vec<Reminder>, Vec<String>), String>;
    fn complete(&mut self, id: &str) -> Result<(), String>;
}

/// In-memory source.
#[derive(Default)]
pub struct FakeReminders {
    pub access: Access,
    pub items: Vec<Reminder>,
    pub completed: Vec<String>,
}

impl RemindersSource for FakeReminders {
    fn access(&self) -> Access {
        self.access
    }
    fn request(&mut self) -> Access {
        self.access = Access::Granted;
        self.access
    }
    fn fetch(&mut self) -> Result<(Vec<Reminder>, Vec<String>), String> {
        let mut lists: Vec<String> = self.items.iter().map(|r| r.list.clone()).collect();
        lists.sort();
        lists.dedup();
        Ok((self.items.iter().filter(|r| !self.completed.contains(&r.id)).cloned().collect(), lists))
    }
    fn complete(&mut self, id: &str) -> Result<(), String> {
        if !self.items.iter().any(|r| r.id == id) {
            return Err("not found".into());
        }
        self.completed.push(id.into());
        Ok(())
    }
}

/// Demo reminders relative to `now`.
pub fn demo(now: f64) -> Vec<Reminder> {
    let blue = Rgba(0.04, 0.52, 1.0, 1.0);
    let orange = Rgba(1.0, 0.62, 0.04, 1.0);
    let purple = Rgba(0.75, 0.35, 0.95, 1.0);
    vec![
        Reminder { id: "r1".into(), title: "Renew passport".into(), list: "Personal".into(), color: orange, due: Some(now - 86_400.0 * 2.0), due_all_day: true, priority: 1 },
        Reminder { id: "r2".into(), title: "Review notch widget PR".into(), list: "Work".into(), color: blue, due: Some(now + 3600.0 * 2.0), due_all_day: false, priority: 0 },
        Reminder { id: "r3".into(), title: "Buy oat milk".into(), list: "Groceries".into(), color: purple, due: None, due_all_day: false, priority: 0 },
        Reminder { id: "r4".into(), title: "Book flights to Lisbon".into(), list: "Personal".into(), color: orange, due: Some(now + 86_400.0 * 3.0), due_all_day: true, priority: 5 },
        Reminder { id: "r5".into(), title: "Call Alex".into(), list: "Work".into(), color: blue, due: None, due_all_day: false, priority: 1 },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mon 2026-10-26 10:00 UTC.
    const NOW: f64 = 1_792_972_800.0 + 36_000.0;

    #[test]
    fn ordering_and_filters() {
        let items = demo(NOW);
        let all = visible(&items, &RemindersSettings::default(), NOW, 0.0, &[]);
        let ids: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["r1", "r2", "r4", "r5", "r3"]);
        let work = RemindersSettings { lists: vec!["work".into()], ..Default::default() };
        assert_eq!(visible(&items, &work, NOW, 0.0, &[]).len(), 2);
        let today = RemindersSettings { show: ReminderFilter::Today, ..Default::default() };
        assert_eq!(visible(&items, &today, NOW, 0.0, &[]).iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["r1", "r2"]);
        let sched = RemindersSettings { show: ReminderFilter::Scheduled, ..Default::default() };
        assert_eq!(visible(&items, &sched, NOW, 0.0, &["r2".into()]).len(), 2);
    }

    #[test]
    fn labels() {
        let items = demo(NOW);
        assert_eq!(due_label(&items[0], NOW, 0.0), ("Overdue".into(), Urgency::Overdue));
        assert_eq!(due_label(&items[1], NOW, 0.0), ("12:00".into(), Urgency::Today));
        assert_eq!(due_label(&items[3], NOW, 0.0), ("Thu".into(), Urgency::Later));
        assert_eq!(due_label(&items[2], NOW, 0.0).1, Urgency::None);
        let late = Reminder { due: Some(NOW - 600.0), ..Default::default() };
        assert_eq!(due_label(&late, NOW, 0.0), ("9:50".into(), Urgency::Overdue));
        let tmrw = Reminder { due: Some(NOW + 86_400.0), due_all_day: true, ..Default::default() };
        assert_eq!(due_label(&tmrw, NOW, 0.0).0, "Tomorrow");
        let far = Reminder { due: Some(NOW + 86_400.0 * 30.0), ..Default::default() };
        assert_eq!(due_label(&far, NOW, 0.0).0, "Nov 25");
        let today_all = Reminder { due: Some(NOW - 3600.0), due_all_day: true, ..Default::default() };
        assert_eq!(due_label(&today_all, NOW, 0.0), ("Today".into(), Urgency::Today));
    }

    #[test]
    fn fake_source() {
        let mut f = FakeReminders { access: Access::NotDetermined, items: demo(NOW), completed: vec![] };
        assert_eq!(f.request(), Access::Granted);
        f.complete("r3").unwrap();
        assert!(f.complete("zz").is_err());
        let (items, lists) = f.fetch().unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(lists, vec!["Groceries", "Personal", "Work"]);
    }
}
