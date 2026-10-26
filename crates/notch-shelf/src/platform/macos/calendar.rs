//! EventKit, loaded at runtime. Lives on the calendar provider thread.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send, sel};

use super::rt;
use crate::widgets::{Access, CalEvent, Rgba};

const EK_ENTITY_EVENT: usize = 0;

fn ek_class() -> Option<&'static objc2::runtime::AnyClass> {
    if rt::class_named("EKEventStore").is_none() {
        rt::load_framework("/System/Library/Frameworks/EventKit.framework");
    }
    rt::class_named("EKEventStore")
}

fn has_usage_key() -> bool {
    rt::info_plist_has("NSCalendarsFullAccessUsageDescription") || rt::info_plist_has("NSCalendarsUsageDescription")
}

/// Current authorization, read-only (never prompts).
pub fn access() -> Access {
    let Some(cls) = ek_class() else { return Access::Unavailable };
    let st: isize = unsafe { msg_send![cls, authorizationStatusForEntityType: EK_ENTITY_EVENT] };
    match st {
        0 if !has_usage_key() => Access::MissingUsageKey,
        0 => Access::NotDetermined,
        // 3 = full access (authorized); 4 = write-only (can't read)
        3 => Access::Granted,
        1 | 2 | 4 => Access::Denied,
        _ => Access::Unknown,
    }
}

pub struct Calendar {
    store: Option<Retained<AnyObject>>,
    /// Set by the permission callback: 0 pending, 1 granted, 2 denied.
    answer: Arc<AtomicU8>,
}

impl Calendar {
    pub fn new() -> Self {
        Calendar { store: None, answer: Arc::new(AtomicU8::new(0)) }
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

    /// Ask for access (shows the system prompt). Only when the Info.plist
    /// has the usage key; otherwise macOS would refuse anyway.
    pub fn request(&mut self) -> Access {
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
            let full: bool = msg_send![store, respondsToSelector: sel!(requestFullAccessToEventsWithCompletion:)];
            if full {
                let _: () = msg_send![store, requestFullAccessToEventsWithCompletion: &*block];
            } else {
                let _: () = msg_send![store, requestAccessToEntityType: EK_ENTITY_EVENT, completion: &*block];
            }
        }
        Access::Requesting
    }

    /// Result of a pending request, if it arrived.
    pub fn poll_answer(&mut self) -> Option<Access> {
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

    /// Events overlapping [from, to) (unix seconds) in all calendars.
    pub fn events(&mut self, from: f64, to: f64) -> Vec<CalEvent> {
        autoreleasepool(|_| {
            let Some(store) = self.store() else { return vec![] };
            unsafe {
                let s: Retained<AnyObject> = msg_send![class!(NSDate), dateWithTimeIntervalSince1970: from];
                let e: Retained<AnyObject> = msg_send![class!(NSDate), dateWithTimeIntervalSince1970: to];
                let pred: Option<Retained<AnyObject>> = msg_send![store, predicateForEventsWithStartDate: &*s, endDate: &*e, calendars: std::ptr::null::<AnyObject>()];
                let Some(pred) = pred else { return vec![] };
                let arr: Option<Retained<AnyObject>> = msg_send![store, eventsMatchingPredicate: &*pred];
                let Some(arr) = arr else { return vec![] };
                let fmt: Retained<AnyObject> = msg_send![class!(NSDateFormatter), new];
                // NSDateFormatterShortStyle = 1, NoStyle = 0
                let _: () = msg_send![&*fmt, setTimeStyle: 1usize];
                let _: () = msg_send![&*fmt, setDateStyle: 0usize];
                let n: usize = msg_send![&*arr, count];
                let mut out = Vec::with_capacity(n.min(64));
                for i in 0..n.min(64) {
                    let ev: Retained<AnyObject> = msg_send![&*arr, objectAtIndex: i];
                    let title: Option<Retained<AnyObject>> = msg_send![&*ev, title];
                    let sd: Option<Retained<AnyObject>> = msg_send![&*ev, startDate];
                    let ed: Option<Retained<AnyObject>> = msg_send![&*ev, endDate];
                    let all_day: bool = msg_send![&*ev, isAllDay];
                    let id: Option<Retained<AnyObject>> = msg_send![&*ev, eventIdentifier];
                    let loc: Option<Retained<AnyObject>> = msg_send![&*ev, location];
                    let cal: Option<Retained<AnyObject>> = msg_send![&*ev, calendar];
                    let (cal_title, color) = match &cal {
                        Some(c) => {
                            let t: Option<Retained<AnyObject>> = msg_send![&**c, title];
                            let col: Option<Retained<AnyObject>> = msg_send![&**c, color];
                            (rt::string_of(t.as_deref()), col.map(|c| srgb(&c)).unwrap_or(Rgba(0.04, 0.52, 1.0, 1.0)))
                        }
                        None => (String::new(), Rgba(0.04, 0.52, 1.0, 1.0)),
                    };
                    let (Some(start), Some(end)) = (rt::unix_of_date(sd.as_deref()), rt::unix_of_date(ed.as_deref())) else { continue };
                    let time_label = match &sd {
                        Some(d) => {
                            let s: Retained<AnyObject> = msg_send![&*fmt, stringFromDate: &**d];
                            rt::string_of(Some(&s))
                        }
                        None => String::new(),
                    };
                    let title = rt::string_of(title.as_deref());
                    out.push(CalEvent {
                        id: rt::string_of(id.as_deref()),
                        title: if title.trim().is_empty() { "New Event".into() } else { title },
                        start,
                        end,
                        all_day,
                        color,
                        calendar: cal_title,
                        location: rt::string_of(loc.as_deref()),
                        time_label,
                    });
                }
                out
            }
        })
    }
}

fn srgb(c: &AnyObject) -> Rgba {
    unsafe {
        let cs: Retained<AnyObject> = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let c: Option<Retained<AnyObject>> = msg_send![c, colorUsingColorSpace: &*cs];
        match c {
            Some(c) => {
                let r: f64 = msg_send![&*c, redComponent];
                let g: f64 = msg_send![&*c, greenComponent];
                let b: f64 = msg_send![&*c, blueComponent];
                Rgba(r, g, b, 1.0)
            }
            None => Rgba(0.04, 0.52, 1.0, 1.0),
        }
    }
}

/// Open Calendar.app.
pub fn open_app() {
    rt::open_path("/System/Applications/Calendar.app");
}

#[cfg(test)]
mod tests {
    #[test]
    fn authorization_status_is_read_only() {
        // Never requests; the test binary has no usage key.
        let a = super::access();
        assert_ne!(a, crate::widgets::Access::Requesting);
        let mut c = super::Calendar::new();
        assert_eq!(c.request(), crate::widgets::Access::MissingUsageKey);
        assert_eq!(c.poll_answer(), None);
    }
}
