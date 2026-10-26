//! Main-thread state: our three status items (chevron, hidden separator,
//! always-hidden separator), the separator length state machine, hover,
//! auto-rehide, focus tracking and the floating bar.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::Arc;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{class, define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::bar::{Bar, BarEntry};
use super::ffi::*;
use super::items;
use crate::logic::{self, Expansion, Hover, HoverAction, Rect, RehideTimer, Section};
use crate::{Config, OwnItem, Request, Shared, Snapshot};

/// Length of a separator while its section is shown.
const SHORT: f64 = 12.0;
/// The classic hiding length (macOS 15 and older).
const LONG: f64 = 10_000.0;
/// A status item window is this much wider than its length.
const PAD: f64 = 16.0;
const SETTLE: f64 = 0.09;
const ANIM_STEPS: usize = 9;
const ANIM_STEP_DT: f64 = 1.0 / 60.0;

pub const AUTOSAVE_TOGGLE: &str = "PowerMacMenuBarChevron";
pub const AUTOSAVE_HIDDEN: &str = "PowerMacMenuBarHiddenSeparator";
pub const AUTOSAVE_ALWAYS: &str = "PowerMacMenuBarAlwaysHiddenSeparator";

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PowerMacMenuBarManagerTarget"]
    pub struct Target;

    impl Target {
        #[unsafe(method(chevronClicked:))]
        fn chevron_clicked(&self, _sender: Option<&AnyObject>) {
            let (option, right) = unsafe {
                let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                let ev: *mut AnyObject = msg_send![app, currentEvent];
                if ev.is_null() {
                    (false, false)
                } else {
                    let flags: usize = msg_send![ev, modifierFlags];
                    let ty: usize = msg_send![ev, type];
                    (flags & (1 << 19) != 0, ty == 3 || ty == 4)
                }
            };
            with(|a| a.on_chevron(option || right));
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

fn status_bar() -> *mut AnyObject {
    unsafe { msg_send![class!(NSStatusBar), systemStatusBar] }
}

fn defaults() -> *mut AnyObject {
    unsafe { msg_send![class!(NSUserDefaults), standardUserDefaults] }
}

fn pref_key(name: &str) -> Retained<objc2_foundation::NSString> {
    ns(&format!("NSStatusItem Preferred Position {name}"))
}

/// Create a status item with an autosave name (macOS restores its place).
fn make_item(name: &str, length: f64) -> Retained<AnyObject> {
    unsafe {
        let item: Retained<AnyObject> = msg_send![status_bar(), statusItemWithLength: length];
        let _: () = msg_send![&*item, setAutosaveName: &*ns(name)];
        // A previous removal may have recorded it as invisible.
        let _: () = msg_send![&*item, setVisible: true];
        item
    }
}

/// Remove a status item, keeping its remembered position.
fn remove_item(name: &str, item: &AnyObject) {
    unsafe {
        let key = pref_key(name);
        let saved: Option<Retained<AnyObject>> = msg_send![defaults(), objectForKey: &*key];
        let _: () = msg_send![status_bar(), removeStatusItem: item];
        if let Some(v) = saved {
            let _: () = msg_send![defaults(), setObject: &*v, forKey: &*key];
        }
        let vis = ns(&format!("NSStatusItem Visible {name}"));
        let _: () = msg_send![defaults(), removeObjectForKey: &*vis];
    }
}

pub fn reset_positions() {
    for n in [AUTOSAVE_TOGGLE, AUTOSAVE_HIDDEN, AUTOSAVE_ALWAYS] {
        unsafe {
            let _: () = msg_send![defaults(), removeObjectForKey: &*pref_key(n)];
            let _: () = msg_send![defaults(), removeObjectForKey: &*ns(&format!("NSStatusItem Visible {n}"))];
        }
    }
}

fn button(item: &AnyObject) -> *mut AnyObject {
    unsafe { msg_send![item, button] }
}

fn item_window(item: &AnyObject) -> *mut AnyObject {
    let b = button(item);
    if b.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { msg_send![b, window] }
}

fn primary_height() -> f64 {
    unsafe {
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        let n: usize = msg_send![screens, count];
        if n == 0 {
            return 0.0;
        }
        let s: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
        let f: NSRect = msg_send![s, frame];
        f.size.height
    }
}

fn to_top_left(f: NSRect, ph: f64) -> Rect {
    Rect::new(f.origin.x, ph - (f.origin.y + f.size.height), f.size.width, f.size.height)
}

fn window_frame(win: *mut AnyObject, ph: f64) -> Option<Rect> {
    if win.is_null() {
        return None;
    }
    let f: NSRect = unsafe { msg_send![win, frame] };
    Some(to_top_left(f, ph))
}

fn item_frame(item: &AnyObject, ph: f64) -> Option<Rect> {
    window_frame(item_window(item), ph)
}

fn symbol(name: &str, desc: &str) -> Option<Retained<AnyObject>> {
    unsafe {
        let img: Option<Retained<AnyObject>> = msg_send![class!(NSImage), imageWithSystemSymbolName: &*ns(name), accessibilityDescription: &*ns(desc)];
        if let Some(i) = &img {
            let _: () = msg_send![&**i, setTemplate: true];
        }
        img
    }
}

/// A thin vertical line (solid or dotted) as a template image.
fn line_image(dotted: bool) -> Retained<AnyObject> {
    unsafe {
        let size = NSSize::new(4.0, 16.0);
        let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
        let img: Retained<AnyObject> = msg_send![a, initWithSize: size];
        let _: () = msg_send![&*img, lockFocus];
        let black: *mut AnyObject = msg_send![class!(NSColor), blackColor];
        let _: () = msg_send![black, setFill];
        if dotted {
            for i in 0..4 {
                let r = NSRect::new(NSPoint::new(1.0, 1.5 + i as f64 * 3.9), NSSize::new(2.0, 2.0));
                let p: *mut AnyObject = msg_send![class!(NSBezierPath), bezierPathWithOvalInRect: r];
                let _: () = msg_send![p, fill];
            }
        } else {
            let r = NSRect::new(NSPoint::new(1.25, 1.0), NSSize::new(1.5, 14.0));
            let p: *mut AnyObject = msg_send![class!(NSBezierPath), bezierPathWithRoundedRect: r, xRadius: 0.75f64, yRadius: 0.75f64];
            let _: () = msg_send![p, fill];
        }
        let _: () = msg_send![&*img, unlockFocus];
        let _: () = msg_send![&*img, setTemplate: true];
        img
    }
}

fn set_image(item: &AnyObject, img: Option<&AnyObject>) {
    let b = button(item);
    if !b.is_null() {
        unsafe {
            let _: () = msg_send![b, setImage: img];
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SepKind {
    Hidden,
    Always,
}

struct Sep {
    kind: SepKind,
    item: Retained<AnyObject>,
    want_long: bool,
    len: f64,
    /// Where the right edge is (window frame) while the separator is short.
    right_ref: Option<f64>,
    /// Correction from the hit-test verification (fitted mode).
    scale: f64,
    verified: bool,
    verify_attempts: u32,
    anim: VecDeque<f64>,
    changed_at: f64,
    image_short: bool,
}

impl Sep {
    fn new(kind: SepKind) -> Sep {
        let name = if kind == SepKind::Hidden { AUTOSAVE_HIDDEN } else { AUTOSAVE_ALWAYS };
        let item = make_item(name, SHORT);
        let b = button(&item);
        if !b.is_null() {
            let tip = if kind == SepKind::Hidden {
                "PowerMac: icons left of this line are hidden (Cmd-drag icons across it)"
            } else {
                "PowerMac: icons left of this line are always hidden"
            };
            unsafe {
                let _: () = msg_send![b, setToolTip: &*ns(tip)];
                let _: () = msg_send![b, setEnabled: false];
            }
        }
        Sep {
            kind,
            item,
            want_long: false,
            len: SHORT,
            right_ref: None,
            scale: 1.0,
            verified: false,
            verify_attempts: 0,
            anim: VecDeque::new(),
            changed_at: now(),
            image_short: false,
        }
    }

    fn name(&self) -> &'static str {
        if self.kind == SepKind::Hidden {
            AUTOSAVE_HIDDEN
        } else {
            AUTOSAVE_ALWAYS
        }
    }

    fn set_len(&mut self, l: f64, t: f64) {
        if (self.len - l).abs() > 0.01 {
            unsafe {
                let _: () = msg_send![&*self.item, setLength: l];
            }
            self.len = l;
            self.changed_at = t;
        }
    }

    fn settled(&self, t: f64) -> bool {
        self.anim.is_empty() && t - self.changed_at >= SETTLE
    }

    fn is_short(&self) -> bool {
        (self.len - SHORT).abs() < 0.01
    }

    fn busy(&self) -> bool {
        !self.anim.is_empty()
    }

    fn update_image(&mut self, show_line: bool) {
        let want = show_line && self.is_short() && self.anim.is_empty();
        if want != self.image_short {
            self.image_short = want;
            let img = want.then(|| line_image(self.kind == SepKind::Always));
            set_image(&self.item, img.as_deref());
        }
    }
}

struct Ctx {
    t: f64,
    ph: f64,
    screen: Rect,
    notch: Option<Rect>,
    menus_right: Option<f64>,
    fitted_mode: bool,
    animate: bool,
}

fn drive(sep: &mut Sep, ctx: &Ctx, right_ref_now: Option<f64>, may_measure: bool, may_go_long: bool) {
    let t = ctx.t;
    if let Some(&l) = sep.anim.front() {
        if t - sep.changed_at >= ANIM_STEP_DT * 0.9 {
            sep.anim.pop_front();
            sep.set_len(l, t);
        }
        return;
    }
    let frame = item_frame(&sep.item, ctx.ph);
    if !sep.want_long {
        sep.verified = false;
        sep.verify_attempts = 0;
        if !sep.is_short() {
            if ctx.animate && sep.len > SHORT {
                let from = if ctx.fitted_mode { sep.len } else { sep.right_ref.map(|r| r - ctx.screen.x).unwrap_or(sep.len).min(sep.len) };
                sep.anim.extend(logic::animation_steps(from, SHORT, ANIM_STEPS));
                // Jump straight to the visible range first.
                let first = sep.anim.pop_front().unwrap_or(SHORT);
                sep.set_len(first, t);
            } else {
                sep.set_len(SHORT, t);
            }
            return;
        }
        if may_measure && sep.settled(t) {
            if let Some(f) = frame {
                if f.x >= ctx.screen.x - 1.0 && f.right() <= ctx.screen.right() + 1.0 {
                    sep.right_ref = Some(right_ref_now.unwrap_or(f.right()));
                }
            }
        }
        return;
    }
    // Wants long.
    if !ctx.fitted_mode {
        if (sep.len - LONG).abs() > 0.01 && may_go_long {
            if ctx.animate && sep.is_short() {
                if let Some(r) = sep.right_ref.or(frame.map(|f| f.right())) {
                    let to = (r - ctx.screen.x - PAD).max(SHORT);
                    sep.anim.extend(logic::animation_steps(SHORT, to, ANIM_STEPS));
                }
            }
            sep.anim.push_back(LONG);
            let first = sep.anim.pop_front().unwrap_or(LONG);
            sep.set_len(first, t);
        }
        return;
    }
    // macOS 26+: the menu bar moves items that don't fit (or would cross
    // the notch) into its overflow, together with everything left of them.
    // So the separator is made just long enough to fill the space between
    // the notch (or the app menus) and its right neighbour: every item left
    // of it would have to cross that limit and is taken out of the bar.
    let Some(r) = right_ref_now.or(sep.right_ref) else {
        // Measure first (needs to be short and laid out).
        if !sep.is_short() {
            sep.set_len(SHORT, t);
        }
        return;
    };
    if !may_go_long {
        return;
    }
    let target = (fitted_length(r, ctx) * sep.scale).clamp(SHORT + 1.0, (r - ctx.screen.x - PAD).max(SHORT + 1.0));
    if (sep.len - target).abs() > 0.5 {
        if ctx.animate && sep.is_short() {
            sep.anim.extend(logic::animation_steps(SHORT, target, ANIM_STEPS));
            let first = sep.anim.pop_front().unwrap_or(target);
            sep.set_len(first, t);
        } else {
            sep.set_len(target, t);
        }
        sep.verified = false;
    }
}

fn fitted_length(r: f64, ctx: &Ctx) -> f64 {
    logic::fitted_length(r, &ctx.screen, ctx.notch.as_ref(), ctx.menus_right, PAD)
}

struct PendingPress {
    id: String,
    at: f64,
    restore: Expansion,
}

pub struct App {
    shared: Arc<Shared>,
    cfg: Config,
    cfg_gen: u64,
    mtm: MainThreadMarker,
    target: Retained<Target>,
    toggle: Option<Retained<AnyObject>>,
    hidden: Option<Sep>,
    always: Option<Sep>,
    /// H right edge minus chevron left edge, measured while short.
    h_offset: Option<f64>,
    expansion: Expansion,
    chevron_state: Option<bool>,
    fitted_mode: bool,
    timer: CFRunLoopTimerRef,
    rehide: RehideTimer,
    hover: Hover,
    front_pid: i32,
    shown_front_pid: i32,
    menu_open: bool,
    last_menu_check: f64,
    menus_right: Option<(i32, Option<f64>)>,
    ph: f64,
    screen: Rect,
    menu_bar_h: f64,
    notch: Option<Rect>,
    last_screen_poll: f64,
    bar: Option<Bar>,
    bar_pending: bool,
    pending_press: Option<PendingPress>,
    verify_in_flight: bool,
    hold: bool,
    last_scan: f64,
    scan_in_flight: Arc<std::sync::atomic::AtomicBool>,
    own_windows: Vec<Retained<AnyObject>>,
    layout_issue: Option<String>,
    last_publish: f64,
    dirty: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` with the app state, unless it's missing or already borrowed.
pub fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

pub fn tick() {
    autoreleasepool(|_| {
        with(|a| a.tick());
    });
}

extern "C" fn timer_cb(_t: CFRunLoopTimerRef, _info: *mut c_void) {
    tick();
}

pub fn install(shared: Arc<Shared>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    uninstall();
    let fitted_mode = super::fitted_mode_default();
    let mut app = App {
        shared,
        cfg: Config::default(),
        cfg_gen: 0,
        mtm,
        target: Target::new(mtm),
        toggle: None,
        hidden: None,
        always: None,
        h_offset: None,
        expansion: Expansion::Collapsed,
        chevron_state: None,
        fitted_mode,
        timer: std::ptr::null(),
        rehide: RehideTimer::default(),
        hover: Hover::default(),
        front_pid: 0,
        shown_front_pid: 0,
        menu_open: false,
        last_menu_check: 0.0,
        menus_right: None,
        ph: primary_height(),
        screen: Rect::default(),
        menu_bar_h: 24.0,
        notch: None,
        last_screen_poll: 0.0,
        bar: None,
        bar_pending: false,
        pending_press: None,
        verify_in_flight: false,
        hold: false,
        last_scan: 0.0,
        scan_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        own_windows: vec![],
        layout_issue: None,
        last_publish: 0.0,
        dirty: true,
    };
    unsafe {
        let t = CFRunLoopTimerCreate(std::ptr::null(), now() + 0.01, 0.25, 0, 0, timer_cb, std::ptr::null_mut());
        CFRunLoopAddTimer(CFRunLoopGetMain(), t, kCFRunLoopCommonModes);
        app.timer = t;
    }
    app.read_screen();
    APP.with(|a| *a.borrow_mut() = Some(app));
    tick();
}

/// Tear down if the running manager is the one owning `shared` (only one
/// manager runs at a time; a newer one replaces an older one).
pub fn uninstall_for(shared: &Arc<Shared>) {
    let mine = with(|a| Arc::ptr_eq(&a.shared, shared)).unwrap_or(false);
    if mine {
        uninstall();
    }
}

pub fn uninstall() {
    let old = APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.take()));
    if let Some(mut app) = old {
        app.teardown();
    }
}

/// Fill a snapshot with the screen / own items without a running manager.
pub fn snapshot_without_manager(mtm: MainThreadMarker) -> Snapshot {
    let _ = mtm;
    let mut s = Snapshot { own_pid: items::own_pid(), ..Default::default() };
    let ph = primary_height();
    if let Some((screen, mbh, notch)) = screen_info(None, ph) {
        s.screen = Some(screen);
        s.menu_bar_h = mbh;
        s.notch = notch;
    }
    let (name, bundle) = own_names();
    s.own_name = name;
    s.own_bundle = bundle;
    let (own, _) = own_status_windows(&[], ph);
    s.own_items = own;
    s
}

fn own_names() -> (String, Option<String>) {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSRunningApplication), currentApplication];
        let name: Option<Retained<objc2_foundation::NSString>> = msg_send![app, localizedName];
        let bundle: Option<Retained<objc2_foundation::NSString>> = msg_send![app, bundleIdentifier];
        (name.map(|n| n.to_string()).unwrap_or_else(|| "PowerMac".into()), bundle.map(|b| b.to_string()))
    }
}

/// The menu bar screen (the one with our chevron, else the primary),
/// menu bar height and notch.
fn screen_info(near: Option<*mut AnyObject>, ph: f64) -> Option<(Rect, f64, Option<Rect>)> {
    unsafe {
        let mut s: *mut AnyObject = std::ptr::null_mut();
        if let Some(w) = near.filter(|w| !w.is_null()) {
            s = msg_send![w, screen];
        }
        if s.is_null() {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            let n: usize = msg_send![screens, count];
            if n == 0 {
                return None;
            }
            s = msg_send![screens, objectAtIndex: 0usize];
        }
        let f: NSRect = msg_send![s, frame];
        let vis: NSRect = msg_send![s, visibleFrame];
        let screen = to_top_left(f, ph);
        let mut mbh = (f.origin.y + f.size.height) - (vis.origin.y + vis.size.height);
        if mbh <= 0.0 {
            let th: f64 = msg_send![status_bar(), thickness];
            mbh = th.max(22.0);
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Insets {
            top: f64,
            left: f64,
            bottom: f64,
            right: f64,
        }
        unsafe impl objc2::encode::Encode for Insets {
            const ENCODING: objc2::encode::Encoding = objc2::encode::Encoding::Struct("NSEdgeInsets", &[f64::ENCODING, f64::ENCODING, f64::ENCODING, f64::ENCODING]);
        }
        let mut notch = None;
        let responds: bool = msg_send![s, respondsToSelector: sel!(safeAreaInsets)];
        if responds {
            let ins: Insets = msg_send![s, safeAreaInsets];
            if ins.top > 0.0 {
                let l: NSRect = msg_send![s, auxiliaryTopLeftArea];
                let r: NSRect = msg_send![s, auxiliaryTopRightArea];
                let _ = (ins.left, ins.bottom, ins.right);
                notch = logic::notch_rect(screen, ins.top.max(mbh), l.origin.x + l.size.width, r.origin.x);
            }
        }
        Some((screen, mbh, notch))
    }
}

/// This process's status item windows except `exclude` (ours), as items,
/// plus the windows themselves (for pressing).
fn own_status_windows(exclude: &[*mut AnyObject], ph: f64) -> (Vec<OwnItem>, Vec<Retained<AnyObject>>) {
    let Some(cls) = AnyClass::get(c"NSStatusBarWindow") else { return (vec![], vec![]) };
    let mut out = vec![];
    let mut wins = vec![];
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let windows: Retained<AnyObject> = msg_send![app, windows];
        let n: usize = msg_send![&*windows, count];
        for i in 0..n {
            let w: *mut AnyObject = msg_send![&*windows, objectAtIndex: i];
            let is: bool = msg_send![w, isKindOfClass: cls];
            if !is || exclude.contains(&w) {
                continue;
            }
            let visible: bool = msg_send![w, isVisible];
            if !visible {
                continue;
            }
            // macOS 26+ keeps replica windows for each item (other displays /
            // Spaces) parked at (0, -height) with no screen; skip them.
            let screen: *mut AnyObject = msg_send![w, screen];
            let f: NSRect = msg_send![w, frame];
            if screen.is_null() && f.origin.x == 0.0 && (f.origin.y + f.size.height).abs() < 0.5 {
                continue;
            }
            let Some(frame) = window_frame(w, ph) else { continue };
            let title = status_button(w).and_then(button_title);
            out.push(OwnItem { title, frame, index: 0 });
            if let Some(r) = Retained::retain(w) {
                wins.push(r);
            }
        }
    }
    let mut idx: Vec<usize> = (0..out.len()).collect();
    idx.sort_by(|&a, &b| out[a].frame.x.total_cmp(&out[b].frame.x));
    let out2: Vec<OwnItem> = idx.iter().enumerate().map(|(k, &i)| OwnItem { index: k, ..out[i].clone() }).collect();
    let wins2: Vec<Retained<AnyObject>> = idx.iter().map(|&i| wins[i].clone()).collect();
    (out2, wins2)
}

/// The NSStatusBarButton inside a status window.
pub fn status_button(w: *mut AnyObject) -> Option<*mut AnyObject> {
    unsafe fn find(v: *mut AnyObject, depth: u32) -> Option<*mut AnyObject> {
        if v.is_null() || depth > 4 {
            return None;
        }
        let is: bool = msg_send![v, isKindOfClass: class!(NSButton)];
        if is {
            return Some(v);
        }
        let subs: *mut AnyObject = msg_send![v, subviews];
        let n: usize = msg_send![subs, count];
        for i in 0..n {
            let s: *mut AnyObject = msg_send![subs, objectAtIndex: i];
            if let Some(b) = find(s, depth + 1) {
                return Some(b);
            }
        }
        None
    }
    unsafe {
        let cv: *mut AnyObject = msg_send![w, contentView];
        find(cv, 0)
    }
}

fn button_title(b: *mut AnyObject) -> Option<String> {
    unsafe {
        let mut candidates: Vec<Option<Retained<objc2_foundation::NSString>>> = vec![];
        candidates.push(msg_send![b, title]);
        candidates.push(msg_send![b, accessibilityTitle]);
        candidates.push(msg_send![b, toolTip]);
        let img: *mut AnyObject = msg_send![b, image];
        if !img.is_null() {
            candidates.push(msg_send![img, accessibilityDescription]);
        }
        candidates.into_iter().flatten().map(|s| s.to_string()).find(|s| !s.is_empty())
    }
}

fn mouse_location(ph: f64) -> (f64, f64) {
    unsafe {
        let p: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        (p.x, ph - p.y)
    }
}

fn pressed_buttons() -> usize {
    unsafe { msg_send![class!(NSEvent), pressedMouseButtons] }
}

fn frontmost_pid() -> i32 {
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut AnyObject = msg_send![ws, frontmostApplication];
        if app.is_null() {
            return 0;
        }
        msg_send![app, processIdentifier]
    }
}

impl App {
    fn active(&self) -> bool {
        self.toggle.is_some() && self.hidden.is_some()
    }

    fn read_screen(&mut self) {
        self.ph = primary_height();
        let near = self.toggle.as_deref().map(item_window);
        if let Some((screen, mbh, notch)) = screen_info(near, self.ph) {
            if screen != self.screen || notch != self.notch {
                // The space changed: fitted lengths must be found again.
                for s in [self.hidden.as_mut(), self.always.as_mut()].into_iter().flatten() {
                    s.scale = 1.0;
                    s.verified = false;
                    s.verify_attempts = 0;
                    s.right_ref = None;
                }
                self.h_offset = None;
            }
            self.screen = screen;
            self.menu_bar_h = mbh;
            self.notch = notch;
        }
        self.last_screen_poll = now();
    }

    fn create_items(&mut self) {
        if self.toggle.is_none() {
            // Created right to left: chevron, hidden separator, always-hidden
            // separator. New items appear at the left end of the status
            // items, so on first run nothing is hidden; afterwards macOS
            // restores the positions from the autosave names.
            let t = make_item(AUTOSAVE_TOGGLE, -1.0);
            let b = button(&t);
            if !b.is_null() {
                unsafe {
                    let _: () = msg_send![b, setTarget: &*self.target];
                    let _: () = msg_send![b, setAction: sel!(chevronClicked:)];
                    // Left and right mouse up.
                    let _: isize = msg_send![b, sendActionOn: (1usize << 2) | (1usize << 4)];
                    let _: () = msg_send![b, setToolTip: &*ns("Show hidden menu bar icons (Option-click: also the always-hidden ones)")];
                }
            }
            self.toggle = Some(t);
            self.chevron_state = None;
        }
        if self.hidden.is_none() {
            self.hidden = Some(Sep::new(SepKind::Hidden));
        }
        if self.cfg.always_hidden_section && self.always.is_none() {
            self.always = Some(Sep::new(SepKind::Always));
        }
        if !self.cfg.always_hidden_section {
            if let Some(a) = self.always.take() {
                remove_item(a.name(), &a.item);
            }
        }
        self.read_screen();
    }

    fn remove_items(&mut self) {
        self.close_bar();
        // Short first, so nothing stays pushed out even for a frame.
        for s in [self.always.as_mut(), self.hidden.as_mut()].into_iter().flatten() {
            s.anim.clear();
            s.set_len(SHORT, now());
        }
        if let Some(a) = self.always.take() {
            remove_item(a.name(), &a.item);
        }
        if let Some(h) = self.hidden.take() {
            remove_item(h.name(), &h.item);
        }
        if let Some(t) = self.toggle.take() {
            remove_item(AUTOSAVE_TOGGLE, &t);
        }
        self.expansion = Expansion::Collapsed;
        self.rehide.cancel();
        self.h_offset = None;
    }

    fn teardown(&mut self) {
        self.remove_items();
        if !self.timer.is_null() {
            unsafe {
                CFRunLoopTimerInvalidate(self.timer);
                CFRelease(self.timer);
            }
            self.timer = std::ptr::null();
        }
        self.publish(true);
    }

    pub fn on_chevron(&mut self, all: bool) {
        self.hover.shown_by_hover = false;
        self.handle(Request::Toggle { all });
        self.tick();
    }

    fn shown(&self) -> bool {
        self.expansion != Expansion::Collapsed || self.bar.is_some()
    }

    fn set_expansion(&mut self, e: Expansion) {
        if !self.active() {
            return;
        }
        let t = now();
        if e != Expansion::Collapsed {
            if self.cfg.auto_rehide {
                self.rehide.start(t, self.cfg.rehide_delay_secs as f64);
            }
            self.shown_front_pid = self.front_pid;
        } else {
            self.rehide.cancel();
            self.hover.shown_by_hover = false;
        }
        self.expansion = e;
        self.dirty = true;
    }

    fn show(&mut self, e: Expansion) {
        if self.cfg.use_floating_bar || self.notch_would_hide(e) {
            self.open_bar(e == Expansion::All);
        } else {
            self.close_bar();
            self.set_expansion(e);
        }
    }

    fn hide(&mut self) {
        self.close_bar();
        self.pending_press = None;
        self.set_expansion(Expansion::Collapsed);
    }

    /// Would expanding in place put items behind the notch? Uses the last
    /// listing (refreshed in the background).
    fn notch_would_hide(&self, e: Expansion) -> bool {
        let (true, Some(notch), Some(h)) = (self.cfg.notch_fallback_to_bar, self.notch, self.hidden.as_ref()) else { return false };
        let Some(toggle) = self.toggle.as_deref().and_then(|t| item_frame(t, self.ph)) else { return false };
        let _ = h;
        let st = self.shared.lock();
        let widths: Vec<f64> = st.last_items.iter().filter(|i| i.section != Section::Visible && e.shows(i.section)).map(|i| i.frame.w).collect();
        let right_edge = toggle.x - SHORT - PAD;
        logic::expansion_overflows_notch(right_edge, &notch, &widths)
    }

    fn handle(&mut self, r: Request) {
        match r {
            Request::Toggle { all } => {
                if self.shown() {
                    self.hide();
                } else {
                    self.show(if all && self.always.is_some() { Expansion::All } else { Expansion::Hidden });
                }
            }
            Request::Show(e) => {
                let e = if e == Expansion::All && self.always.is_none() { Expansion::Hidden } else { e };
                if e == Expansion::Collapsed {
                    self.hide();
                } else {
                    self.close_bar();
                    self.set_expansion(e);
                }
            }
            Request::Hide => self.hide(),
            Request::ShowBar { all } => self.open_bar(all),
            Request::Press(id) => self.start_press(id),
            Request::Refresh => self.dirty = true,
            Request::Hold(h) => {
                self.hold = h;
                if !h && self.shown() && self.cfg.auto_rehide {
                    self.rehide.start(now(), self.cfg.rehide_delay_secs as f64);
                }
            }
        }
    }

    fn start_press(&mut self, id: String) {
        if !self.active() {
            return;
        }
        let section = {
            let st = self.shared.lock();
            st.last_items.iter().find(|i| i.id == id).map(|i| i.section).or_else(|| st.section_cache.get(&id).copied())
        };
        let restore = if self.bar.is_some() { Expansion::Collapsed } else { self.expansion };
        self.close_bar();
        let need = match section {
            Some(Section::AlwaysHidden) => Expansion::All,
            Some(Section::Hidden) | None => {
                if self.expansion == Expansion::All {
                    Expansion::All
                } else {
                    Expansion::Hidden
                }
            }
            Some(Section::Visible) => self.expansion,
        };
        // Press without animation: just show it.
        let animate = self.cfg.animate;
        self.cfg.animate = false;
        self.set_expansion(need);
        self.step_separators(now());
        self.cfg.animate = animate;
        self.pending_press = Some(PendingPress { id, at: now() + 0.3, restore });
    }

    fn do_press(&mut self, p: PendingPress) {
        let items = self.shared.lock().last_items.clone();
        let Some(item) = items.into_iter().find(|i| i.id == p.id) else {
            log::info!("menubar-manager: no item {} to press", p.id);
            return;
        };
        if item.own {
            let ph = self.ph;
            let ptrs: Vec<*mut AnyObject> = self.own_windows.iter().map(|w| Retained::as_ptr(w) as *mut AnyObject).collect();
            let by_title = ptrs.iter().find(|&&p| item.title.is_some() && status_button(p).and_then(button_title) == item.title);
            let by_frame = ptrs.iter().find(|&&p| window_frame(p, ph).is_some_and(|f| (f.x - item.frame.x).abs() < 2.0 && (f.w - item.frame.w).abs() < 2.0));
            if let Some(&w) = by_title.or(by_frame) {
                if let Some(b) = status_button(w) {
                    unsafe {
                        let _: () = msg_send![b, performClick: std::ptr::null::<AnyObject>()];
                    }
                }
            }
        } else {
            let id = item.id.clone();
            std::thread::spawn(move || {
                let pressed = items::find_ax_item(item.pid, &id, Some(item.frame)).map(|e| e.press()).unwrap_or(false);
                if !pressed {
                    // Fall back to a click, but only if the item is really there.
                    let (cx, cy) = (item.frame.center_x(), item.frame.center_y());
                    if items::pid_at(cx, cy) == Some(item.pid) {
                        items::click_at(cx, cy);
                    } else {
                        log::info!("menubar-manager: couldn't press {id}");
                    }
                }
            });
        }
        if self.expansion != p.restore {
            // Rehide shortly after the menu closes.
            self.rehide.start(now(), 2.0);
        }
    }

    fn open_bar(&mut self, all: bool) {
        if !self.active() || self.bar_pending {
            return;
        }
        self.bar_pending = true;
        let shared = self.shared.clone();
        let snap = self.snapshot();
        let screen_recording = unsafe { CGPreflightScreenCaptureAccess() };
        std::thread::spawn(move || {
            let items = crate::refresh_items(&shared, &snap);
            let notch_hidden: Vec<String> = crate::hidden_by_notch(&snap, &items).into_iter().map(|i| i.id).collect();
            let entries: Vec<BarEntry> = items
                .into_iter()
                .filter(|i| matches!(i.section, Section::Hidden) || (all && i.section == Section::AlwaysHidden) || notch_hidden.contains(&i.id))
                .map(|i| {
                    let image = if screen_recording { i.window_id.and_then(window_image).map(|p| p as usize) } else { None };
                    BarEntry { item: i, cg_image: image }
                })
                .collect();
            dispatch_main(move || {
                let leftover = with(|a| a.show_bar_entries(entries));
                if let Some(Some(v)) = leftover {
                    for e in v {
                        if let Some(p) = e.cg_image {
                            unsafe { CGImageRelease(p as *const c_void) };
                        }
                    }
                }
            });
        });
    }

    fn show_bar_entries(&mut self, entries: Vec<BarEntry>) -> Option<Vec<BarEntry>> {
        self.bar_pending = false;
        if !self.active() {
            return Some(entries);
        }
        self.close_bar();
        let anchor = self.toggle.as_deref().and_then(|t| item_frame(t, self.ph)).unwrap_or_default();
        let appearance: *mut AnyObject = self.toggle.as_deref().map(button).filter(|b| !b.is_null()).map(|b| unsafe { msg_send![b, effectiveAppearance] }).unwrap_or(std::ptr::null_mut());
        self.bar = Bar::open(self.mtm, entries, &self.own_windows, anchor, self.screen, self.menu_bar_h, self.ph, appearance);
        if self.bar.is_some() {
            if self.cfg.auto_rehide {
                self.rehide.start(now(), self.cfg.rehide_delay_secs as f64);
            }
            self.shown_front_pid = self.front_pid;
            self.dirty = true;
        }
        None
    }

    fn close_bar(&mut self) {
        if let Some(b) = self.bar.take() {
            b.close();
            self.dirty = true;
        }
    }

    /// macOS 26+: check with hit tests (what the menu bar really shows)
    /// that nothing is left visible to the left of a long separator, and
    /// lengthen it if something is. Runs off the main thread (hit tests on
    /// our own items are answered by the main thread).
    fn verify_fitted(&mut self, t: f64) {
        if !self.fitted_mode || self.verify_in_flight || !unsafe { AXIsProcessTrusted() } {
            return;
        }
        let h_short = self.hidden.as_ref().is_some_and(|h| h.is_short() && h.settled(t));
        let toggle_x = self.toggle.as_deref().and_then(|x| item_frame(x, self.ph)).map(|f| f.x);
        let y = self.screen.y + self.menu_bar_h / 2.0;
        let mut job = None;
        for kind in [SepKind::Hidden, SepKind::Always] {
            let sep = if kind == SepKind::Hidden { self.hidden.as_ref() } else { self.always.as_ref() };
            let Some(sep) = sep else { continue };
            if !sep.want_long || sep.verified || sep.is_short() || !sep.settled(t) {
                continue;
            }
            if kind == SepKind::Always && !h_short {
                continue;
            }
            let r = if kind == SepKind::Hidden { toggle_x.zip(self.h_offset).map(|(x, o)| x + o).or(sep.right_ref) } else { sep.right_ref };
            let Some(r) = r else { continue };
            job = Some((kind, sep.len, r - (sep.len + PAD) / 2.0, r - sep.len - PAD - 10.0));
            break;
        }
        let Some((kind, len, center, leak_x)) = job else { return };
        self.verify_in_flight = true;
        let own = items::own_pid();
        std::thread::spawn(move || {
            let on_sep = items::hit(center, y);
            let left = if leak_x > 0.0 { items::hit(leak_x, y) } else { None };
            let visible = on_sep.as_ref().is_some_and(|h| h.pid == own);
            let leak = left.as_ref().is_some_and(|h| h.status_item);
            dispatch_main(move || {
                with(|a| a.on_verified(kind, len, visible, leak));
            });
        });
    }

    fn on_verified(&mut self, kind: SepKind, len: f64, visible: bool, leak: bool) {
        self.verify_in_flight = false;
        let sep = if kind == SepKind::Hidden { self.hidden.as_mut() } else { self.always.as_mut() };
        let Some(sep) = sep else { return };
        if (sep.len - len).abs() > 0.5 || !sep.want_long {
            return;
        }
        sep.verify_attempts += 1;
        log::debug!("menubar-manager: {kind:?} separator at {len:.0}: shown {visible}, item visible left of it {leak}");
        if leak && sep.verify_attempts < 5 {
            // Something still fits left of it: make it longer.
            sep.scale *= 1.25;
            self.dirty = true;
        } else {
            if leak {
                log::warn!("menubar-manager: couldn't push every item out of the menu bar");
            }
            sep.verified = true;
        }
    }

    fn background_scan(&mut self, t: f64) {
        use std::sync::atomic::Ordering;
        if self.scan_in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        self.last_scan = t;
        self.publish(true);
        let snap = self.snapshot();
        let shared = self.shared.clone();
        let flag = self.scan_in_flight.clone();
        std::thread::spawn(move || {
            crate::refresh_items(&shared, &snap);
            flag.store(false, Ordering::SeqCst);
        });
    }

    pub fn bar_clicked(&mut self, id: String) {
        self.start_press(id);
    }

    fn apply_config(&mut self) {
        let (cfg, gen, stopped) = {
            let st = self.shared.lock();
            (st.config.clone(), st.config_gen, st.stopped)
        };
        if gen == self.cfg_gen {
            return;
        }
        self.cfg_gen = gen;
        let was_enabled = self.active();
        self.cfg = cfg;
        if self.cfg.enabled && !stopped {
            self.create_items();
            if !was_enabled {
                self.set_expansion(Expansion::Collapsed);
            }
        } else if was_enabled {
            self.remove_items();
        }
        if !self.cfg.use_floating_bar && self.bar.is_some() && !self.cfg.notch_fallback_to_bar {
            self.close_bar();
        }
        if !self.cfg.auto_rehide {
            self.rehide.cancel();
        }
        self.dirty = true;
    }

    fn step_separators(&mut self, t: f64) {
        let toggle_frame = self.toggle.as_deref().and_then(|x| item_frame(x, self.ph));
        let ctx = Ctx {
            t,
            ph: self.ph,
            screen: self.screen,
            notch: self.notch,
            menus_right: self.menus_right.and_then(|m| m.1),
            fitted_mode: self.fitted_mode,
            animate: self.cfg.animate,
        };
        let e = self.expansion;
        let always_busy = false;
        let h_short_settled = self.hidden.as_ref().is_some_and(|h| h.is_short() && h.settled(t));
        if let Some(h) = self.hidden.as_mut() {
            h.want_long = e == Expansion::Collapsed;
            // The hidden separator's right neighbour is the chevron.
            if h.is_short() && h.settled(t) {
                if let (Some(tf), Some(hf)) = (toggle_frame, item_frame(&h.item, self.ph)) {
                    if hf.x >= self.screen.x && hf.right() <= tf.x + 4.0 {
                        self.h_offset = Some(hf.right() - tf.x);
                    }
                }
            }
            let r = match (toggle_frame, self.h_offset) {
                (Some(tf), Some(o)) => Some(tf.x + o),
                _ => None,
            };
            // Don't push the always-hidden separator out while it is being fitted.
            drive(h, &ctx, r, true, !always_busy);
            h.update_image(self.cfg.show_separators);
        }
        if let Some(a) = self.always.as_mut() {
            a.want_long = e != Expansion::All;
            // Its frame is only real while the hidden separator is short.
            let hidden_frame = self.hidden.as_ref().and_then(|h| item_frame(&h.item, self.ph));
            let sane = h_short_settled && hidden_frame.is_some();
            if sane && a.is_short() && a.settled(t) {
                if let (Some(af), Some(hf)) = (item_frame(&a.item, self.ph), hidden_frame) {
                    if af.right() > hf.x + 2.0 {
                        // Out of order (dragged right of the hidden separator).
                        a.right_ref = None;
                    }
                }
            }
            drive(a, &ctx, None, sane, true);
            a.update_image(self.cfg.show_separators);
        }
        // Chevron image follows what is shown.
        let shown = self.shown();
        if self.chevron_state != Some(shown) {
            if let Some(tg) = self.toggle.as_deref() {
                let img = if shown { symbol("chevron.compact.right", "Hide menu bar icons").or_else(|| symbol("chevron.right", "Hide")) } else { symbol("chevron.compact.left", "Show hidden menu bar icons").or_else(|| symbol("chevron.left", "Show")) };
                set_image(tg, img.as_deref());
                self.chevron_state = Some(shown);
            }
        }
    }

    fn busy(&self) -> bool {
        self.hidden.as_ref().is_some_and(|s| s.busy()) || self.always.as_ref().is_some_and(|s| s.busy())
    }

    fn check_order(&mut self, t: f64) {
        let (Some(tg), Some(h)) = (self.toggle.as_deref(), self.hidden.as_ref()) else {
            self.layout_issue = None;
            return;
        };
        if self.expansion != Expansion::All || self.busy() || !h.settled(t) {
            return;
        }
        let (Some(tf), Some(hf)) = (item_frame(tg, self.ph), item_frame(&h.item, self.ph)) else { return };
        let af = self.always.as_ref().and_then(|a| item_frame(&a.item, self.ph));
        let issue = logic::order_problem(tf, hf, af).map(str::to_string);
        if issue != self.layout_issue {
            if let Some(i) = &issue {
                log::warn!("menubar-manager: {i}");
            }
            self.layout_issue = issue;
            self.dirty = true;
        }
    }

    fn snapshot(&self) -> Snapshot {
        let ph = self.ph;
        let toggle = self.toggle.as_deref().and_then(|x| item_frame(x, ph));
        let hidden = self.hidden.as_ref().and_then(|x| item_frame(&x.item, ph));
        let always = self.always.as_ref().and_then(|x| item_frame(&x.item, ph));
        let t = now();
        let h_short = self.hidden.as_ref().is_some_and(|h| h.is_short() && h.settled(t));
        let always_reliable = always.is_some() && (!self.fitted_mode || h_short) && always.zip(hidden).is_some_and(|(a, h)| a.right() <= h.x + 2.0 || !self.fitted_mode);
        let mut exclude = vec![];
        for it in [self.toggle.as_deref(), self.hidden.as_ref().map(|s| &*s.item), self.always.as_ref().map(|s| &*s.item)].into_iter().flatten() {
            exclude.push(item_window(it));
        }
        let (own_items, _) = own_status_windows(&exclude, ph);
        let (own_name, own_bundle) = own_names();
        Snapshot {
            gen: 0,
            active: self.active(),
            expansion: self.expansion,
            bar_open: self.bar.is_some(),
            fitted_mode: self.fitted_mode,
            busy: self.busy() || self.pending_press.is_some(),
            toggle,
            hidden,
            always_hidden: always,
            always_hidden_reliable: always_reliable,
            screen: Some(self.screen),
            menu_bar_h: self.menu_bar_h,
            notch: self.notch,
            own_items,
            own_name,
            own_bundle,
            own_pid: items::own_pid(),
            layout_issue: self.layout_issue.clone(),
        }
    }

    fn publish(&mut self, force: bool) {
        let t = now();
        if !force && !self.dirty && t - self.last_publish < 0.5 {
            return;
        }
        let mut exclude = vec![];
        for it in [self.toggle.as_deref(), self.hidden.as_ref().map(|s| &*s.item), self.always.as_ref().map(|s| &*s.item)].into_iter().flatten() {
            exclude.push(item_window(it));
        }
        self.own_windows = own_status_windows(&exclude, self.ph).1;
        let snap = self.snapshot();
        self.shared.publish(snap);
        self.last_publish = t;
        self.dirty = false;
    }

    pub fn refresh(&mut self) {
        self.publish(true);
    }

    pub fn tick(&mut self) {
        let t = now();
        self.apply_config();
        let reqs = std::mem::take(&mut self.shared.lock().requests);
        for r in reqs {
            self.handle(r);
        }
        if !self.active() {
            self.publish(false);
            self.schedule(1.0);
            return;
        }
        if t - self.last_screen_poll > 2.0 {
            self.read_screen();
        }
        let was_busy = self.busy();
        self.step_separators(t);

        // Frontmost app (focus) and app menus extent.
        let front = frontmost_pid();
        if front != self.front_pid {
            self.front_pid = front;
            if self.shown() && !self.hold && self.cfg.rehide_on_focus_change && front != self.shown_front_pid && front != items::own_pid() && !self.menu_open && self.pending_press.is_none() {
                self.hide();
            }
            if self.notch.is_none() && (self.cfg.show_on_hover || self.fitted_mode) {
                self.menus_right = Some((front, items::app_menus_right(front)));
                if self.fitted_mode {
                    // The free space changed: check the length again.
                    for s in [self.hidden.as_mut(), self.always.as_mut()].into_iter().flatten() {
                        s.scale = 1.0;
                        s.verified = false;
                        s.verify_attempts = 0;
                    }
                }
            }
        }

        let shown = self.shown();
        if shown && t - self.last_menu_check > 0.4 {
            self.menu_open = items::menu_open();
            self.last_menu_check = t;
        }
        let (mx, my) = mouse_location(self.ph);
        let in_menu_bar = self.screen.contains(mx, my) && my < self.screen.y + self.menu_bar_h + 1.0;
        let in_bar = self.bar.as_ref().is_some_and(|b| b.contains(mx, my));

        // A click outside the floating bar closes it.
        if self.bar.is_some() && !in_bar && pressed_buttons() & 1 != 0 && !in_menu_bar {
            self.close_bar();
            self.rehide.cancel();
        }

        // Hover.
        if self.cfg.show_on_hover && !self.hold {
            let toggle_x = self.toggle.as_deref().and_then(|x| item_frame(x, self.ph)).map(|f| f.x).unwrap_or(self.screen.right());
            let left_limit = match (self.notch, self.menus_right.and_then(|m| m.1)) {
                (Some(n), _) => n.right(),
                (None, Some(m)) => m,
                _ => self.hidden.as_ref().and_then(|h| item_frame(&h.item, self.ph)).map(|f| f.x.max(self.screen.x + self.screen.w * 0.3)).unwrap_or(self.screen.x + self.screen.w * 0.5),
            };
            let over_empty = in_menu_bar && mx > left_limit && mx < toggle_x - 2.0 && pressed_buttons() == 0;
            let dwell = self.cfg.hover_delay_ms.min(5000) as f64 / 1000.0;
            let input = logic::HoverInput { over_empty_bar: over_empty, in_menu_bar: in_menu_bar || in_bar, shown, menu_open: self.menu_open };
            match self.hover.tick(t, input, dwell, 0.6) {
                HoverAction::Show => {
                    self.show(Expansion::Hidden);
                    self.hover.shown_by_hover = true;
                }
                HoverAction::Hide => self.hide(),
                HoverAction::None => {}
            }
        }

        // Auto-rehide.
        if self.shown() && self.cfg.auto_rehide || self.rehide.armed() {
            let activity = in_menu_bar || in_bar || self.menu_open || self.pending_press.is_some() || self.hold;
            if self.rehide.tick(t, self.cfg.rehide_delay_secs as f64, activity) && self.shown() {
                self.hide();
            }
        }

        // A pending press from the floating bar.
        if let Some(p) = self.pending_press.as_ref() {
            if t >= p.at && !self.busy() {
                let p = self.pending_press.take().unwrap();
                // Fresh frames for the press.
                self.publish(true);
                let shared = self.shared.clone();
                let snap = self.snapshot();
                // Own items are known here; others need a listing (off the
                // main thread), then come back to press.
                std::thread::spawn(move || {
                    crate::refresh_items(&shared, &snap);
                    dispatch_main(move || {
                        with(|a| a.do_press(p));
                    });
                });
            }
        }

        self.verify_fitted(t);
        self.check_order(t);
        if was_busy != self.busy() {
            self.dirty = true;
        }
        // Keep the listing fresh in the background (sections for the
        // floating-bar / notch decisions): after the layout settles, and
        // every 30 s.
        let settled_now = was_busy && !self.busy();
        if (settled_now && t - self.last_scan > 1.0) || t - self.last_scan > 30.0 {
            self.background_scan(t);
        }
        self.publish(false);
        let next = if self.busy() {
            ANIM_STEP_DT
        } else if self.pending_press.is_some() || self.bar.is_some() || self.cfg.show_on_hover || shown {
            0.1
        } else {
            0.5
        };
        self.schedule(next);
    }

    fn schedule(&self, dt: f64) {
        if !self.timer.is_null() {
            unsafe { CFRunLoopTimerSetNextFireDate(self.timer, now() + dt) };
        }
    }
}
