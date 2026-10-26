//! Reminders through EventKit (loaded at runtime). Used from the widget
//! runner thread only. Access is requested lazily (first time the card is
//! shown, or "Allow Access"), and only when the host's Info.plist has the
//! usage description (`NSRemindersFullAccessUsageDescription` on macOS 14+,
//! `NSRemindersUsageDescription` before); without it macOS would refuse.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send, sel};

use super::rt;
use crate::widgets::reminders::{Reminder, RemindersSource};
use crate::widgets::{Access, Rgba};

const EK_ENTITY_REMINDER: usize = 1;
/// NSDateComponentUndefined.
const UNDEFINED: isize = isize::MAX;

fn ek_class() -> Option<&'static objc2::runtime::AnyClass> {
    if rt::class_named("EKEventStore").is_none() {
        rt::load_framework("/System/Library/Frameworks/EventKit.framework");
    }
    rt::class_named("EKEventStore")
}

fn has_usage_key() -> bool {
    rt::info_plist_has("NSRemindersFullAccessUsageDescription") || rt::info_plist_has("NSRemindersUsageDescription")
}

pub struct EkReminders {
    store: Option<Retained<AnyObject>>,
    /// Set by the permission callback: 0 pending, 1 granted, 2 denied.
    answer: Arc<AtomicU8>,
}

impl EkReminders {
    pub fn new() -> Self {
        EkReminders { store: None, answer: Arc::new(AtomicU8::new(0)) }
    }

    fn store(&mut self) -> Option<&AnyObject> {
        if self.store.is_none() {
            let cls = ek_class()?;
            self.store = unsafe {
                let a: objc2::rc::Allocated<AnyObject> = msg_send![cls, alloc];
                msg_send![a, init]
            };
        }
        self.store.as_deref()
    }
}

impl RemindersSource for EkReminders {
    /// Current authorization, read-only (never prompts).
    fn access(&self) -> Access {
        let Some(cls) = ek_class() else { return Access::Unavailable };
        let st: isize = unsafe { msg_send![cls, authorizationStatusForEntityType: EK_ENTITY_REMINDER] };
        match st {
            0 if !has_usage_key() => Access::MissingUsageKey,
            0 => Access::NotDetermined,
            3 => Access::Granted,
            1 | 2 | 4 => Access::Denied,
            _ => Access::Unknown,
        }
    }

    fn request(&mut self) -> Access {
        if !has_usage_key() {
            return Access::MissingUsageKey;
        }
        let answer = self.answer.clone();
        answer.store(0, Ordering::SeqCst);
        let Some(store) = self.store() else { return Access::Unavailable };
        let block = RcBlock::new(move |granted: Bool, _err: *mut AnyObject| {
            answer.store(if granted.as_bool() { 1 } else { 2 }, Ordering::SeqCst);
        });
        unsafe {
            let full: bool = msg_send![store, respondsToSelector: sel!(requestFullAccessToRemindersWithCompletion:)];
            if full {
                let _: () = msg_send![store, requestFullAccessToRemindersWithCompletion: &*block];
            } else {
                let _: () = msg_send![store, requestAccessToEntityType: EK_ENTITY_REMINDER, completion: &*block];
            }
        }
        Access::Requesting
    }

    fn poll_answer(&mut self) -> Option<Access> {
        match self.answer.swap(0, Ordering::SeqCst) {
            1 => {
                // A store created before access was granted sees nothing.
                self.store = None;
                Some(Access::Granted)
            }
            2 => Some(Access::Denied),
            _ => None,
        }
    }

    fn fetch(&mut self) -> Result<(Vec<Reminder>, Vec<String>), String> {
        let (tx, rx) = mpsc::channel::<Vec<Reminder>>();
        let lists = autoreleasepool(|_| -> Result<Vec<String>, String> {
            let store = self.store().ok_or("EventKit isn't available")?;
            unsafe {
                let cals: Option<Retained<AnyObject>> = msg_send![store, calendarsForEntityType: EK_ENTITY_REMINDER];
                let mut lists = vec![];
                if let Some(cals) = &cals {
                    let n: usize = msg_send![&**cals, count];
                    for i in 0..n {
                        let c: Retained<AnyObject> = msg_send![&**cals, objectAtIndex: i];
                        let t: Option<Retained<AnyObject>> = msg_send![&*c, title];
                        lists.push(rt::string_of(t.as_deref()));
                    }
                }
                let nil = std::ptr::null::<AnyObject>();
                let pred: Option<Retained<AnyObject>> = msg_send![store, predicateForIncompleteRemindersWithDueDateStarting: nil, ending: nil, calendars: nil];
                let pred = pred.ok_or("No predicate")?;
                let block = RcBlock::new(move |arr: *mut AnyObject| {
                    let v = if arr.is_null() { vec![] } else { autoreleasepool(|_| convert(&*arr)) };
                    let _ = tx.send(v);
                });
                let _: Retained<AnyObject> = msg_send![store, fetchRemindersMatchingPredicate: &*pred, completion: &*block];
                Ok(lists)
            }
        })?;
        let items = rx.recv_timeout(Duration::from_secs(15)).map_err(|_| "Reminders didn't answer".to_string())?;
        let mut lists = lists;
        lists.sort();
        lists.dedup();
        Ok((items, lists))
    }

    fn complete(&mut self, id: &str) -> Result<(), String> {
        autoreleasepool(|_| {
            let store = self.store().ok_or("EventKit isn't available")?;
            unsafe {
                let item: Option<Retained<AnyObject>> = msg_send![store, calendarItemWithIdentifier: &*super::ffi::ns(id)];
                let item = item.ok_or("Reminder not found")?;
                let _: () = msg_send![&*item, setCompleted: true];
                let mut err: *mut AnyObject = std::ptr::null_mut();
                let ok: bool = msg_send![store, saveReminder: &*item, commit: true, error: &mut err];
                if ok {
                    Ok(())
                } else if !err.is_null() {
                    let d: Retained<objc2_foundation::NSString> = msg_send![&*err, localizedDescription];
                    Err(d.to_string())
                } else {
                    Err("Couldn't save".into())
                }
            }
        })
    }
}

/// EKReminder array → reminders (on EventKit's callback thread).
unsafe fn convert(arr: &AnyObject) -> Vec<Reminder> {
    let n: usize = msg_send![arr, count];
    let cal: Retained<AnyObject> = msg_send![class!(NSCalendar), currentCalendar];
    let mut out = Vec::with_capacity(n.min(200));
    for i in 0..n.min(200) {
        let r: Retained<AnyObject> = msg_send![arr, objectAtIndex: i];
        let title: Option<Retained<AnyObject>> = msg_send![&*r, title];
        let id: Option<Retained<AnyObject>> = msg_send![&*r, calendarItemIdentifier];
        let list: Option<Retained<AnyObject>> = msg_send![&*r, calendar];
        let (list_name, color) = match &list {
            Some(c) => {
                let t: Option<Retained<AnyObject>> = msg_send![&**c, title];
                let col: Option<Retained<AnyObject>> = msg_send![&**c, color];
                (rt::string_of(t.as_deref()), col.map(|c| super::calendar::srgb(&c)).unwrap_or(Rgba(1.0, 0.62, 0.04, 1.0)))
            }
            None => (String::new(), Rgba(1.0, 0.62, 0.04, 1.0)),
        };
        let comps: Option<Retained<AnyObject>> = msg_send![&*r, dueDateComponents];
        let (due, all_day) = match &comps {
            Some(c) => {
                let hour: isize = msg_send![&**c, hour];
                let d: Option<Retained<AnyObject>> = msg_send![&*cal, dateFromComponents: &**c];
                (rt::unix_of_date(d.as_deref()), hour == UNDEFINED)
            }
            None => (None, false),
        };
        let priority: usize = msg_send![&*r, priority];
        let title = rt::string_of(title.as_deref());
        out.push(Reminder {
            id: rt::string_of(id.as_deref()),
            title: if title.trim().is_empty() { "New Reminder".into() } else { title },
            list: list_name,
            color,
            due,
            due_all_day: all_day,
            priority: priority.min(9) as u8,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_read_only_and_requests_need_the_usage_key() {
        // Never prompts: the test binary has no usage key.
        let mut r = EkReminders::new();
        assert_ne!(r.access(), Access::Requesting);
        assert_eq!(r.request(), Access::MissingUsageKey);
        assert_eq!(r.poll_answer(), None);
    }
}
