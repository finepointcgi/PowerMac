//! The shelf window and its view: mouse, scroll, drag destination and drag
//! source. Events are forwarded to [`super::app`]; anything that runs a
//! nested event loop or a session (menus, drags, share pickers, the folder
//! chooser, moving the window) is started after the app state is released.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSEvent, NSEventModifierFlags, NSPanel, NSResponder, NSView, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSArray, NSObject, NSPoint, NSRect};

use pm_shelf_core::geom::{Point, Rect};
use pm_shelf_core::mac::ffi::ns;
use pm_shelf_core::mac::{gfx, pboard};

use super::app::{self, Deferred, Mods};

define_class!(
    /// A borderless, non-activating panel that never becomes key or main:
    /// clicking a shelf leaves the app you are in in front.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    pub struct ShelfWindow;

    impl ShelfWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            false
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            false
        }
    }
);

impl ShelfWindow {
    pub fn new(mtm: MainThreadMarker, rect: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        unsafe { msg_send![super(this), initWithContentRect: rect, styleMask: style, backing: NSBackingStoreType::Buffered, defer: false] }
    }
}

pub struct Ivars {
    shelf: Cell<u64>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct ShelfView;

    impl ShelfView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &NSEvent) {
            let p = self.local(e);
            let d = app::with(|a| a.on_mouse_down(self.id(), p, mods(e), e.clickCount())).flatten();
            self.run(d, e);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, e: &NSEvent) {
            let p = self.local(e);
            let d = app::with(|a| a.on_mouse_dragged(self.id(), p)).flatten();
            self.run(d, e);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, e: &NSEvent) {
            let p = self.local(e);
            let d = app::with(|a| a.on_mouse_up(self.id(), p)).flatten();
            self.run(d, e);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, e: &NSEvent) {
            let p = self.local(e);
            let d = app::with(|a| a.on_right_down(self.id(), p)).flatten();
            self.run(d, e);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, e: &NSEvent) {
            let (dx, dy) = (e.scrollingDeltaX(), e.scrollingDeltaY());
            let precise = e.hasPreciseScrollingDeltas();
            app::with(|a| a.on_scroll(self.id(), dx, dy, precise));
        }

        // NSDraggingDestination

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, info: &AnyObject) -> usize {
            self.drag_update(info)
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, info: &AnyObject) -> usize {
            self.drag_update(info)
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _info: Option<&AnyObject>) {
            app::with(|a| a.on_drag_hover(self.id(), false));
        }

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag(&self, _info: &AnyObject) -> bool {
            true
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, info: &AnyObject) -> bool {
            let pb: Retained<AnyObject> = unsafe { msg_send![info, draggingPasteboard] };
            app::with(|a| a.on_drop(self.id(), &pb)).unwrap_or(false)
        }

        #[unsafe(method(concludeDragOperation:))]
        fn conclude_drag(&self, _info: Option<&AnyObject>) {
            app::with(|a| a.on_drag_hover(self.id(), false));
        }

        // NSDraggingSource

        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_mask(&self, _s: &AnyObject, ctx: isize) -> usize {
            // Other apps: copy / link / generic; other shelves (inside the
            // app): copy / generic.
            if ctx == 0 {
                1 | 2 | 4
            } else {
                1 | 4
            }
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn drag_ended(&self, _s: &AnyObject, _p: NSPoint, op: usize) {
            app::with(|a| a.on_drag_out_ended(self.id(), op));
        }

        #[unsafe(method(ignoreModifierKeysForDraggingSession:))]
        fn ignore_modifiers(&self, _s: &AnyObject) -> bool {
            false
        }

        // Menus / sharing

        #[unsafe(method(menuAction:))]
        fn menu_action(&self, item: &AnyObject) {
            let tag: isize = unsafe { msg_send![item, tag] };
            let d = app::with(|a| a.on_menu(tag)).flatten();
            if let Some(d) = d {
                self.run_deferred(d, None);
            }
        }

        #[unsafe(method(sharingServicePicker:didChooseSharingService:))]
        fn picker_chose(&self, _p: &AnyObject, _s: Option<&AnyObject>) {
            app::with(|a| a.set_sharing(false));
        }
    }
);

fn mods(e: &NSEvent) -> Mods {
    let f = e.modifierFlags();
    Mods { cmd: f.contains(NSEventModifierFlags::Command), shift: f.contains(NSEventModifierFlags::Shift) }
}

impl ShelfView {
    pub fn new(mtm: MainThreadMarker, frame: NSRect, shelf: u64) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { shelf: Cell::new(shelf) });
        let v: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let types: Vec<_> = pm_shelf_core::uti::ACCEPTED.iter().map(|t| ns(t)).collect();
        let arr = NSArray::from_retained_slice(&types);
        unsafe {
            let _: () = msg_send![&*v, registerForDraggedTypes: &*arr];
        }
        v
    }

    fn id(&self) -> u64 {
        self.ivars().shelf.get()
    }

    /// Window-local top-left point of an event.
    fn local(&self, e: &NSEvent) -> Point {
        let lw = e.locationInWindow();
        let h = self.bounds().size.height;
        Point::new(lw.x, h - lw.y)
    }

    /// Window-local top-left rect to AppKit's y-up view coordinates.
    fn to_view(&self, r: Rect) -> NSRect {
        let h = self.bounds().size.height;
        gfx::nsrect(Rect::new(r.x, h - (r.y + r.h), r.w, r.h))
    }

    fn drag_update(&self, info: &AnyObject) -> usize {
        let source: Option<Retained<AnyObject>> = unsafe { msg_send![info, draggingSource] };
        let own = source.is_some_and(|s| std::ptr::eq(&*s as *const AnyObject, self as *const Self as *const AnyObject));
        let loc: NSPoint = unsafe { msg_send![info, draggingLocation] };
        let h = self.bounds().size.height;
        let p = Point::new(loc.x, h - loc.y);
        let pb: Retained<AnyObject> = unsafe { msg_send![info, draggingPasteboard] };
        let ok = !own && pboard::accepts(&pboard::types_of(&pb));
        let inside = app::with(|a| a.on_drag_over(self.id(), p, ok)).unwrap_or(false);
        if ok && inside {
            1 // NSDragOperationCopy
        } else {
            0
        }
    }

    fn run(&self, d: Option<Deferred>, e: &NSEvent) {
        if let Some(d) = d {
            self.run_deferred(d, Some(e));
        }
    }

    fn run_deferred(&self, d: Deferred, e: Option<&NSEvent>) {
        match d {
            Deferred::MoveWindow => {
                if let (Some(e), Some(w)) = (e, self.window()) {
                    unsafe {
                        let _: () = msg_send![&*w, performWindowDragWithEvent: e];
                    }
                }
            }
            Deferred::DragOut(items) => match e {
                Some(e) => self.begin_drag(items, e),
                None => {
                    app::with(|a| a.on_drag_out_ended(self.id(), 0));
                }
            },
            Deferred::Menu { entries, at } => self.popup(entries, at),
            Deferred::Share { items, anchor } => self.share(items, anchor),
            Deferred::ChooseFolder { mv, count } => {
                let dest = choose_folder(mv, count);
                app::with(|a| a.on_folder_chosen(dest));
            }
        }
    }

    fn begin_drag(&self, items: Vec<(Retained<AnyObject>, Rect, Retained<AnyObject>)>, e: &NSEvent) {
        let mut out = vec![];
        for (writer, frame, image) in items {
            unsafe {
                let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSDraggingItem), alloc];
                let di: Retained<AnyObject> = msg_send![a, initWithPasteboardWriter: &*writer];
                let _: () = msg_send![&*di, setDraggingFrame: self.to_view(frame), contents: &*image];
                out.push(di);
            }
        }
        if out.is_empty() {
            app::with(|a| a.on_drag_out_ended(self.id(), 0));
            return;
        }
        let n = out.len();
        let arr = NSArray::from_retained_slice(&out);
        unsafe {
            let session: Retained<AnyObject> = msg_send![self, beginDraggingSessionWithItems: &*arr, event: e, source: self];
            let _: () = msg_send![&*session, setAnimatesToStartingPositionsOnCancelOrFail: true];
            if n > 1 {
                // NSDraggingFormationStack
                let _: () = msg_send![&*session, setDraggingFormation: 2isize];
            }
        }
    }

    fn share(&self, items: Vec<Retained<AnyObject>>, anchor: Rect) {
        if items.is_empty() {
            app::with(|a| a.set_sharing(false));
            return;
        }
        let arr = NSArray::from_retained_slice(&items);
        unsafe {
            let a: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSSharingServicePicker), alloc];
            let picker: Retained<AnyObject> = msg_send![a, initWithItems: &*arr];
            let _: () = msg_send![&*picker, setDelegate: self];
            app::with(|a| a.keep_picker(picker.clone()));
            // NSMinYEdge (1): below the anchor.
            let _: () = msg_send![&*picker, showRelativeToRect: self.to_view(anchor), ofView: self, preferredEdge: 1usize];
        }
    }

    fn popup(&self, entries: Vec<(String, isize, &'static str, bool)>, at: Point) {
        unsafe {
            let menu: Retained<AnyObject> = msg_send![class!(NSMenu), new];
            let _: () = msg_send![&*menu, setAutoenablesItems: false];
            for (title, tag, symbol, enabled) in entries {
                if tag < 0 {
                    let sep: Retained<AnyObject> = msg_send![class!(NSMenuItem), separatorItem];
                    let _: () = msg_send![&*menu, addItem: &*sep];
                    continue;
                }
                let item: Retained<AnyObject> = msg_send![&*menu, addItemWithTitle: &*ns(&title), action: sel!(menuAction:), keyEquivalent: &*ns("")];
                let _: () = msg_send![&*item, setTarget: self];
                let _: () = msg_send![&*item, setTag: tag];
                let _: () = msg_send![&*item, setEnabled: enabled];
                if let Some(img) = gfx::symbol_nsimage(symbol, 13.0, gfx::W_REGULAR) {
                    let _: () = msg_send![&*item, setImage: &*img];
                }
            }
            app::with(|a| a.set_menu_open(true));
            let h = self.bounds().size.height;
            let local = NSPoint::new(at.x, h - at.y);
            let _: bool = msg_send![&*menu, popUpMenuPositioningItem: std::ptr::null::<AnyObject>(), atLocation: local, inView: self];
            app::with(|a| a.set_menu_open(false));
        }
    }
}

/// Ask for a folder with an open panel. PowerMac is a menu-bar app that
/// isn't active, so the panel only gets the keyboard if the app is
/// activated first; the app that was in front gets it back afterwards.
fn choose_folder(mv: bool, count: usize) -> Option<std::path::PathBuf> {
    let mtm = MainThreadMarker::new()?;
    unsafe {
        let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let prev: Option<Retained<AnyObject>> = msg_send![&*ws, frontmostApplication];
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        let panel: Retained<AnyObject> = msg_send![class!(NSOpenPanel), openPanel];
        let _: () = msg_send![&*panel, setCanChooseFiles: false];
        let _: () = msg_send![&*panel, setCanChooseDirectories: true];
        let _: () = msg_send![&*panel, setCanCreateDirectories: true];
        let _: () = msg_send![&*panel, setAllowsMultipleSelection: false];
        let what = if count == 1 { "1 item".to_string() } else { format!("{count} items") };
        let (prompt, msg) = if mv { ("Move Here", format!("Choose where to move {what}")) } else { ("Copy Here", format!("Choose where to copy {what}")) };
        let _: () = msg_send![&*panel, setPrompt: &*ns(prompt)];
        let _: () = msg_send![&*panel, setMessage: &*ns(&msg)];
        // Above the shelves.
        let _: () = msg_send![&*panel, setLevel: super::app::LEVEL + 1];
        let r: isize = msg_send![&*panel, runModal];
        let out = if r == 1 {
            let url: Option<Retained<AnyObject>> = msg_send![&*panel, URL];
            url.and_then(|u| {
                let p: Option<Retained<objc2_foundation::NSString>> = msg_send![&*u, path];
                p.map(|p| std::path::PathBuf::from(p.to_string()))
            })
        } else {
            None
        };
        if let Some(prev) = prev {
            let me: Retained<AnyObject> = msg_send![class!(NSRunningApplication), currentApplication];
            let mine: bool = msg_send![&*prev, isEqual: &*me];
            if !mine {
                // NSApplicationActivateAllWindows = 1
                let _: bool = msg_send![&*prev, activateWithOptions: 1usize];
            }
        }
        out
    }
}

/// Make a shelf view the window's content: layer-hosting, so the layer tree
/// (y-up, never flipped) is ours on screen and in offscreen renders alike.
pub fn host(view: &ShelfView, root: &AnyObject) {
    unsafe {
        let _: () = msg_send![view, setLayer: root];
    }
    let v: &NSView = view;
    v.setWantsLayer(true);
}
