//! One global input hook for every PowerMac module.
//!
//! Before this crate each module installed its own `CGEventTap` (macOS) or
//! `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks (Windows) on its own thread: about
//! ten taps, each woken for every event, ordered by *creation time* (a tap
//! created later sits in front), each disabled by timeout on its own. Now
//! there is exactly one tap per platform, owned by this crate, and modules
//! [`subscribe`] to it:
//!
//! ```no_run
//! use pm_hook::{EventMask, EventType, Subscription, Verdict};
//! let guard = pm_hook::subscribe(
//!     Subscription::new("example", 100, EventMask::KEYS, |ev| {
//!         if ev.kind() == EventType::KeyDown { Verdict::Pass } else { Verdict::Pass }
//!     })
//!     .ignore_tags(&[pm_hook::tags::MWB]),
//! );
//! // ... dropping the guard unsubscribes.
//! drop(guard);
//! ```
//!
//! # Platforms
//!
//! * **macOS:** one active `CGEventTap` at `kCGHIDEventTap` (falling back to
//!   `kCGSessionEventTap`), head-inserted, on the `pm-hook` thread's run
//!   loop. Its event mask is the union of the subscribers' masks (key
//!   down/up, flagsChanged, every mouse button / drag, moves, scroll,
//!   `NSSystemDefined` for media keys) and is recreated when that union
//!   changes; with no subscribers there is no tap at all. Without
//!   Accessibility a listen-only tap is used when possible (verdicts are then
//!   ignored) and an active tap is retried every 2 s.
//! * **Windows:** one `WH_KEYBOARD_LL` and one `WH_MOUSE_LL` hook, each only
//!   while some subscriber wants keyboard / mouse events, on the `pm-hook`
//!   thread with its own message loop. LL hooks can't edit events:
//!   [`Verdict::Modified`] acts like `Pass`, and [`Verdict::Replace`] swallows
//!   the event and `SendInput`s the replacement, which comes back through the
//!   hook as an injected event (so every subscriber sees it, as with
//!   separate hooks). Handlers whose state belongs to a GUI thread can hop
//!   there synchronously with `win::Forwarder` (Windows only).
//!
//! # Dispatch
//!
//! Subscribers are called in **descending priority** (ties: earlier
//! subscription first) on the tap thread, each only for the event types in
//! its [`EventMask`] and not for events whose [`Event::source_tag`] is in its
//! `ignore_tags`. A handler returns a [`Verdict`]:
//!
//! * `Pass` — continue with the next subscriber.
//! * `Modified` — the handler edited the native event in place (macOS:
//!   `CGEventSet…`); later subscribers and the OS see the edited event.
//! * `Swallow` — the event is dropped; later subscribers never see it.
//! * `Replace(emits)` — the event is replaced by `emits`, in order. Each
//!   [`Emit::Event`] is a synthetic event; [`Emit::Original`] stands for the
//!   event itself (so "post these first, then let the event through" is
//!   `Replace(vec![a, b, Emit::Original])`). On macOS every emitted event is
//!   first dispatched to the *lower*-priority subscribers (exactly as an
//!   event posted with `CGEventTapPostEvent` reached the taps behind the
//!   posting tap) and then posted right after the tap. Higher-priority
//!   subscribers never see them.
//!
//! A panicking handler counts as `Pass`. Subscribing and unsubscribing are
//! allowed from inside handlers: dispatch works on a snapshot, a new
//! subscriber gets the next event, and an unsubscribed one is never called
//! again (dropping a [`Guard`] from another thread waits for a call in
//! progress to finish).
//!
//! # Priorities
//!
//! The order reproduces what the separate taps did when the app started
//! (the deterministic case): on macOS the taps at `kCGHIDEventTap` (Keyboard
//! Manager, Scroll Reverser, Mouse Without Borders, mouse buttons) all ran
//! before the `kCGSessionEventTap` ones (everything else), and inside each
//! group the module started last ran first. The model behind it:
//!
//! 1. **Transforms** rewrite the physical input into what the user meant —
//!    key remaps, the Hyper key, wheel direction. They come first, so every
//!    later consumer (and a remote machine) sees the remapped keys and the
//!    corrected wheel, the way PowerToys' Keyboard Manager output is what
//!    FancyZones and the rest react to.
//! 2. **Routing:** Mouse Without Borders. While it controls another machine
//!    it swallows everything, so no local shortcut fires.
//! 3. **Local consumers:** button actions, text expansion, panels and
//!    hotkeys. They swallow only what they act on.
//!
//! | priority | subscriber | events | why here |
//! |---:|---|---|---|
//! | 1000 | color-picker (Windows, during a pick) | mouse, keys | modal: owns clicks and Esc while picking (its hooks used to be installed last, i.e. first) |
//! | 900 | keyboard-manager | keys, flags, buttons, drags, scroll | remaps / Hyper / flag rewriting before anyone matches a shortcut |
//! | 850 | scroll-reverser | scroll | wheel direction and smoothing before routing and before FancyZones' wheel |
//! | 800 | mwb (pm-input capture) | everything | swallows all input while controlling a remote machine |
//! | 700 | mouse-tuning buttons | other-mouse buttons | button actions are local; a remote machine gets the real button |
//! | 650 | gestures (click watcher) | left button, drags | tells taps from clicks; swallows the click of a bound N-finger click |
//! | 600 | text-expander | key down, clicks | sees the remapped keys; its typed replacement reaches the panels below |
//! | 550 | dock-previews | mouse, scroll, key down | its panel's keys / clicks |
//! | 500 | window-switcher | keys, flags, moves, left button | Option+Tab, Cmd+Tab take-over |
//! | 450 | clipboard-history (macOS hotkey) | keys | Cmd+Shift+V |
//! | 400 | display-control | keys, NSSystemDefined | brightness / volume keys |
//! | 300 | fancyzones | buttons, drags, scroll, keys, flags | drag + hotkeys, wheel while dragging |
//! | 200 | mouse-utils | moves, buttons, keys, flags | observes nearly everything; swallows only its own hotkeys |
//!
//! Constants: [`priority`]. Tags of injected events: [`tags`].
//!
//! # Health
//!
//! [`status`] / [`Subscription::on_status`] report the tap state
//! ([`TapState`]), Secure Event Input and how often the tap had to be
//! re-enabled. The tap thread re-enables a tap disabled by timeout or user
//! input at once, checks it every 0.5 s ([`is_healthy`] checks it on
//! demand), and logs handlers slower than [`SLOW_HANDLER`] ([`stats`]).
//! Handlers must be fast: hand anything slow to a [`Worker`].

use std::sync::Arc;
use std::time::Duration;

mod hub;
pub mod testing;
mod worker;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
pub mod win;

pub use hub::{Guard, Hub, SubscriberStats};
pub use worker::{defer, Worker};

/// Handlers slower than this are logged (rate-limited) and counted.
pub const SLOW_HANDLER: Duration = Duration::from_millis(10);

/// Tags stamped on injected events (macOS `kCGEventSourceUserData`, Windows
/// `dwExtraInfo`), so each subscriber can skip what it must skip.
pub mod tags {
    /// "PMMW": `pm-input` injection (Mouse Without Borders' remote input,
    /// mouse-tuning's keystrokes, a few modules' helper keys).
    pub const MWB: usize = 0x504D_4D57;
    /// "PMKM": Keyboard Manager's remapped keys and typed text.
    pub const KEYBOARD_MANAGER: usize = 0x504D_4B4D;
    /// "PMTE": the text expander's backspaces, text and paste.
    pub const TEXT_EXPANDER: usize = 0x504D_5445;
    /// "SRVS": Scroll Reverser's re-injected / smoothed wheel events.
    pub const SCROLL_REVERSER: usize = 0x5352_5653;
    /// "MTUN": events mouse-tuning injects itself.
    pub const MOUSE_TUNING: usize = 0x4D54_554E;

    /// A readable name for a known tag.
    pub fn name(tag: usize) -> Option<&'static str> {
        Some(match tag {
            MWB => "PMMW",
            KEYBOARD_MANAGER => "PMKM",
            TEXT_EXPANDER => "PMTE",
            SCROLL_REVERSER => "SRVS",
            MOUSE_TUNING => "MTUN",
            _ => return None,
        })
    }
}

/// Subscriber priorities (higher runs first). See the table in the crate docs.
pub mod priority {
    pub const COLOR_PICKER: i32 = 1000;
    pub const KEYBOARD_MANAGER: i32 = 900;
    pub const SCROLL_REVERSER: i32 = 850;
    pub const MOUSE_WITHOUT_BORDERS: i32 = 800;
    pub const MOUSE_TUNING: i32 = 700;
    /// Trackpad gestures' click watcher (N-finger clicks; `crates/gestures`).
    pub const GESTURES: i32 = 650;
    pub const TEXT_EXPANDER: i32 = 600;
    pub const DOCK_PREVIEWS: i32 = 550;
    pub const WINDOW_SWITCHER: i32 = 500;
    pub const CLIPBOARD_HISTORY: i32 = 450;
    pub const DISPLAY_CONTROL: i32 = 400;
    pub const FANCYZONES: i32 = 300;
    pub const MOUSE_UTILS: i32 = 200;
}

/// Platform-neutral event types. macOS maps them 1:1 to `CGEventType`;
/// Windows maps `WM_KEYDOWN`/`WM_SYSKEYDOWN` → `KeyDown`, `WM_MOUSEMOVE` →
/// `MouseMoved` (also while a button is held: there are no drag messages),
/// middle and X buttons → `OtherMouse…`, both wheels → `ScrollWheel`.
/// `FlagsChanged` and `SystemDefined` only exist on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EventType {
    KeyDown = 0,
    KeyUp,
    FlagsChanged,
    MouseMoved,
    LeftMouseDown,
    LeftMouseUp,
    RightMouseDown,
    RightMouseUp,
    OtherMouseDown,
    OtherMouseUp,
    LeftMouseDragged,
    RightMouseDragged,
    OtherMouseDragged,
    ScrollWheel,
    /// `NSSystemDefined` (media / brightness keys).
    SystemDefined,
}

impl EventType {
    pub const ALL: [EventType; 15] = [
        EventType::KeyDown,
        EventType::KeyUp,
        EventType::FlagsChanged,
        EventType::MouseMoved,
        EventType::LeftMouseDown,
        EventType::LeftMouseUp,
        EventType::RightMouseDown,
        EventType::RightMouseUp,
        EventType::OtherMouseDown,
        EventType::OtherMouseUp,
        EventType::LeftMouseDragged,
        EventType::RightMouseDragged,
        EventType::OtherMouseDragged,
        EventType::ScrollWheel,
        EventType::SystemDefined,
    ];

    pub fn is_keyboard(self) -> bool {
        matches!(self, EventType::KeyDown | EventType::KeyUp | EventType::FlagsChanged | EventType::SystemDefined)
    }

    pub fn is_mouse(self) -> bool {
        !self.is_keyboard()
    }
}

/// A set of [`EventType`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EventMask(u32);

impl EventMask {
    pub const NONE: EventMask = EventMask(0);
    pub const KEYS: EventMask = EventMask::of(&[EventType::KeyDown, EventType::KeyUp, EventType::FlagsChanged]);
    pub const MOUSE_BUTTONS: EventMask = EventMask::of(&[
        EventType::LeftMouseDown,
        EventType::LeftMouseUp,
        EventType::RightMouseDown,
        EventType::RightMouseUp,
        EventType::OtherMouseDown,
        EventType::OtherMouseUp,
    ]);
    pub const DRAGS: EventMask =
        EventMask::of(&[EventType::LeftMouseDragged, EventType::RightMouseDragged, EventType::OtherMouseDragged]);
    pub const MOVES: EventMask = EventMask::of(&[EventType::MouseMoved]).union(EventMask::DRAGS);
    pub const SCROLL: EventMask = EventMask::of(&[EventType::ScrollWheel]);
    pub const ALL: EventMask = EventMask::of(&EventType::ALL);

    pub const fn of(types: &[EventType]) -> EventMask {
        let mut m = 0u32;
        let mut i = 0;
        while i < types.len() {
            m |= 1 << types[i] as u8;
            i += 1;
        }
        EventMask(m)
    }

    pub const fn union(self, o: EventMask) -> EventMask {
        EventMask(self.0 | o.0)
    }

    pub const fn with(self, t: EventType) -> EventMask {
        EventMask(self.0 | 1 << t as u8)
    }

    pub const fn contains(self, t: EventType) -> bool {
        self.0 & (1 << t as u8) != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn has_keyboard(self) -> bool {
        EventType::ALL.iter().any(|t| t.is_keyboard() && self.contains(*t))
    }

    pub fn has_mouse(self) -> bool {
        EventType::ALL.iter().any(|t| t.is_mouse() && self.contains(*t))
    }

    pub fn types(self) -> impl Iterator<Item = EventType> {
        EventType::ALL.into_iter().filter(move |t| self.contains(*t))
    }
}

impl std::ops::BitOr for EventMask {
    type Output = EventMask;
    fn bitor(self, o: EventMask) -> EventMask {
        self.union(o)
    }
}

impl std::ops::BitOr<EventType> for EventMask {
    type Output = EventMask;
    fn bitor(self, t: EventType) -> EventMask {
        self.with(t)
    }
}

impl From<EventType> for EventMask {
    fn from(t: EventType) -> EventMask {
        EventMask::NONE.with(t)
    }
}

/// What a handler wants done with an event.
#[derive(Debug)]
pub enum Verdict {
    Pass,
    Swallow,
    /// Edited in place (macOS); Windows treats it as `Pass`.
    Modified,
    Replace(Vec<Emit>),
}

/// One element of a [`Verdict::Replace`].
#[derive(Debug)]
pub enum Emit {
    /// The event being handled (at most once per `Replace`).
    Original,
    Event(Synthetic),
}

/// An owned synthetic event.
pub enum Synthetic {
    /// A `CGEventRef` this value owns (+1 retain; released when dropped).
    #[cfg(target_os = "macos")]
    Mac(macos::OwnedEvent),
    /// An `INPUT` for `SendInput`.
    #[cfg(target_os = "windows")]
    Input(windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT),
    /// Tests: an event of the fake backend.
    Fake(testing::FakeEvent),
}

impl std::fmt::Debug for Synthetic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(target_os = "macos")]
            Synthetic::Mac(e) => write!(f, "Synthetic::Mac({:p})", e.as_ptr()),
            #[cfg(target_os = "windows")]
            Synthetic::Input(i) => write!(f, "Synthetic::Input(type {})", i.r#type),
            Synthetic::Fake(e) => write!(f, "Synthetic::Fake({e:?})"),
        }
    }
}

// SAFETY: a CGEvent is a CF object that may be used from any thread once
// handed over; INPUT is plain data.
unsafe impl Send for Synthetic {}

impl Synthetic {
    /// Take ownership of a `CGEventRef` (as returned by `CGEventCreate…` /
    /// `CGEventCreateCopy`). Null yields `None`.
    ///
    /// # Safety
    /// `ev` must be a valid, +1 retained `CGEventRef` (or null).
    #[cfg(target_os = "macos")]
    pub unsafe fn from_cg(ev: *mut std::ffi::c_void) -> Option<Synthetic> {
        macos::OwnedEvent::from_raw(ev).map(Synthetic::Mac)
    }

    #[cfg(target_os = "windows")]
    pub fn input(i: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT) -> Synthetic {
        Synthetic::Input(i)
    }
}

/// The event view handed to handlers.
pub struct Event<'a> {
    kind: EventType,
    tag: usize,
    synthetic: bool,
    raw: Raw<'a>,
}

pub(crate) enum Raw<'a> {
    #[cfg(target_os = "macos")]
    Mac { ev: *mut std::ffi::c_void, ty: u32, _l: std::marker::PhantomData<&'a ()> },
    #[cfg(target_os = "windows")]
    Key { msg: u32, info: &'a windows_sys::Win32::UI::WindowsAndMessaging::KBDLLHOOKSTRUCT },
    #[cfg(target_os = "windows")]
    Mouse { msg: u32, info: &'a windows_sys::Win32::UI::WindowsAndMessaging::MSLLHOOKSTRUCT },
    Fake(&'a mut testing::FakeEvent),
}

impl<'a> Event<'a> {
    pub(crate) fn new(kind: EventType, tag: usize, synthetic: bool, raw: Raw<'a>) -> Self {
        Event { kind, tag, synthetic, raw }
    }

    pub fn kind(&self) -> EventType {
        self.kind
    }

    /// The injection tag: macOS `kCGEventSourceUserData`; Windows
    /// `dwExtraInfo` of an injected event (0 for hardware events).
    pub fn source_tag(&self) -> usize {
        self.tag
    }

    pub fn has_tag(&self, tags: &[usize]) -> bool {
        tags.contains(&self.tag)
    }

    /// True for events another subscriber emitted with
    /// [`Verdict::Replace`] during this dispatch (macOS).
    pub fn is_synthetic(&self) -> bool {
        self.synthetic
    }

    /// The raw `CGEventRef` (valid for the duration of the call).
    #[cfg(target_os = "macos")]
    pub fn cg_event(&self) -> *mut std::ffi::c_void {
        match &self.raw {
            Raw::Mac { ev, .. } => *ev,
            _ => std::ptr::null_mut(),
        }
    }

    /// The `CGEventType` number.
    #[cfg(target_os = "macos")]
    pub fn cg_type(&self) -> u32 {
        match &self.raw {
            Raw::Mac { ty, .. } => *ty,
            _ => u32::MAX,
        }
    }

    /// `CGEventGetFlags`.
    #[cfg(target_os = "macos")]
    pub fn flags(&self) -> u64 {
        let e = self.cg_event();
        if e.is_null() {
            0
        } else {
            unsafe { macos::CGEventGetFlags(e) }
        }
    }

    /// `CGEventGetIntegerValueField`.
    #[cfg(target_os = "macos")]
    pub fn integer_field(&self, field: u32) -> i64 {
        let e = self.cg_event();
        if e.is_null() {
            0
        } else {
            unsafe { macos::CGEventGetIntegerValueField(e, field) }
        }
    }

    /// `CGEventGetLocation` (global display points, top-left origin).
    #[cfg(target_os = "macos")]
    pub fn location(&self) -> (f64, f64) {
        let e = self.cg_event();
        if e.is_null() {
            return (0.0, 0.0);
        }
        let p = unsafe { macos::CGEventGetLocation(e) };
        (p.x, p.y)
    }

    /// The message (`WM_KEYDOWN`, …) and struct of a keyboard event.
    #[cfg(target_os = "windows")]
    pub fn keyboard(&self) -> Option<(u32, &windows_sys::Win32::UI::WindowsAndMessaging::KBDLLHOOKSTRUCT)> {
        match &self.raw {
            Raw::Key { msg, info } => Some((*msg, info)),
            _ => None,
        }
    }

    /// The message (`WM_MOUSEMOVE`, …) and struct of a mouse event.
    #[cfg(target_os = "windows")]
    pub fn mouse(&self) -> Option<(u32, &windows_sys::Win32::UI::WindowsAndMessaging::MSLLHOOKSTRUCT)> {
        match &self.raw {
            Raw::Mouse { msg, info } => Some((*msg, info)),
            _ => None,
        }
    }

    /// Windows: the `LLKHF_INJECTED` / `LLMHF_INJECTED` flag.
    #[cfg(target_os = "windows")]
    pub fn is_injected(&self) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{LLKHF_INJECTED, LLMHF_INJECTED};
        match &self.raw {
            Raw::Key { info, .. } => info.flags & LLKHF_INJECTED != 0,
            Raw::Mouse { info, .. } => info.flags & LLMHF_INJECTED != 0,
            _ => false,
        }
    }

    /// The fake backend's event (tests).
    pub fn fake(&mut self) -> Option<&mut testing::FakeEvent> {
        match &mut self.raw {
            Raw::Fake(e) => Some(e),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }
}

/// Handler type.
pub type Handler = Box<dyn FnMut(&mut Event<'_>) -> Verdict + Send>;
/// Status callback type.
pub type StatusFn = Arc<dyn Fn(&Status) + Send + Sync>;

/// A subscription request for [`subscribe`].
pub struct Subscription {
    pub name: String,
    pub priority: i32,
    pub mask: EventMask,
    pub handler: Handler,
    /// Events carrying one of these tags are not passed to the handler.
    pub ignore_tags: Vec<usize>,
    /// Called with the current status right after subscribing (on the
    /// subscribing thread) and on every change (on a pm-hook thread). Must
    /// be quick and must not wait for the tap thread.
    pub on_status: Option<StatusFn>,
    /// Also run the handler while only a listen-only tap exists (macOS
    /// without Accessibility; verdicts are ignored then). Off by default:
    /// a module that can't swallow its shortcuts shouldn't act on them.
    pub listen_only: bool,
}

impl Subscription {
    pub fn new(
        name: impl Into<String>,
        priority: i32,
        mask: EventMask,
        handler: impl FnMut(&mut Event<'_>) -> Verdict + Send + 'static,
    ) -> Self {
        Subscription {
            name: name.into(),
            priority,
            mask,
            handler: Box::new(handler),
            ignore_tags: Vec::new(),
            on_status: None,
            listen_only: false,
        }
    }

    /// See [`Subscription::listen_only`].
    pub fn allow_listen_only(mut self) -> Self {
        self.listen_only = true;
        self
    }

    pub fn ignore_tags(mut self, tags: &[usize]) -> Self {
        self.ignore_tags.extend_from_slice(tags);
        self
    }

    pub fn on_status(mut self, f: impl Fn(&Status) + Send + Sync + 'static) -> Self {
        self.on_status = Some(Arc::new(f));
        self
    }
}

/// State of the global tap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapState {
    /// No subscribers (no tap).
    Idle,
    /// Being created.
    Starting,
    /// An active (filtering) tap / hooks are installed.
    Active,
    /// Only a listen-only tap could be created (macOS without
    /// Accessibility): handlers run but verdicts are ignored. `keyboard`
    /// tells whether key events are delivered (Input Monitoring).
    ListenOnly { keyboard: bool },
    /// The OS disabled the tap and re-enabling it failed (so far).
    Disabled,
    /// No tap could be created; retried every 2 s.
    Unavailable(String),
}

/// Health of the global tap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: TapState,
    /// macOS Secure Event Input (password fields): key events don't reach
    /// any tap while it is on.
    pub secure_input: bool,
    /// How often the tap was found disabled and re-enabled.
    pub reenabled: u64,
}

impl Status {
    pub fn is_active(&self) -> bool {
        self.state == TapState::Active
    }
}

impl Default for Status {
    fn default() -> Self {
        Status { state: TapState::Idle, secure_input: false, reenabled: 0 }
    }
}

/// The process-wide hub with the platform backend.
pub fn global() -> &'static Arc<Hub> {
    use std::sync::OnceLock;
    static HUB: OnceLock<Arc<Hub>> = OnceLock::new();
    HUB.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let b: Box<dyn hub::Backend> = Box::new(macos::MacBackend::new());
        #[cfg(target_os = "windows")]
        let b: Box<dyn hub::Backend> = Box::new(win::WinBackend::new());
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let b: Box<dyn hub::Backend> = Box::new(hub::NoBackend);
        Hub::with_backend(b)
    })
}

/// Subscribe to the global tap. Creates the tap if needed and waits (up to
/// a few seconds) until it covers `sub.mask`, so [`status`] is meaningful
/// right after this returns (except when called on the tap thread itself).
pub fn subscribe(sub: Subscription) -> Guard {
    global().subscribe(sub)
}

/// Current status of the global tap.
pub fn status() -> Status {
    global().status()
}

/// Whether the global tap is active and enabled right now. macOS checks
/// `CGEventTapIsEnabled` (re-enabling a silently disabled tap once).
pub fn is_healthy() -> bool {
    global().is_healthy()
}

/// Diagnostics / tests: disable the global tap as macOS does on a timeout,
/// without the "disabled" notification (the watchdog must notice). False
/// when there is no active tap (or on Windows).
#[doc(hidden)]
pub fn debug_disable_tap() -> bool {
    global().debug_disable_tap()
}

/// macOS `IsSecureEventInputEnabled()` (live); false elsewhere.
pub fn secure_input_enabled() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::secure_input()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Per-subscriber timing of the global hub.
pub fn stats() -> Vec<SubscriberStats> {
    global().stats()
}

/// Names and priorities of the current subscribers, in dispatch order.
pub fn subscribers() -> Vec<(String, i32)> {
    global().subscribers()
}
