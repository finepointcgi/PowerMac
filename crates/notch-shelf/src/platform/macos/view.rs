//! The panel's content view: mouse, scroll, drag destination and drag
//! source. It only translates events and forwards them to the [`super::app`]
//! state; anything that runs a nested event loop (menus, drag sessions,
//! share pickers) is started *after* the app state is released.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSEvent, NSEventModifierFlags, NSPanel, NSResponder, NSView, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSArray, NSObject, NSPoint, NSRect};

use super::app::{self, Mods};
use super::ffi::ns;
use super::gfx::nsrect;
use super::pboard;
use crate::geometry::Point;

pub struct Ivars {
    pub panel: Cell<usize>,
}

pub struct PanelIvars {
    key_ok: Cell<bool>,
}

define_class!(
    /// The shelf window: a non-activating panel that may become key only
    /// while the note is being edited (so typing goes to it without
    /// activating PowerMac or taking focus otherwise).
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PanelIvars]
    pub struct ShelfPanel;

    impl ShelfPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            self.ivars().key_ok.get()
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            false
        }
    }
);

impl ShelfPanel {
    pub fn new(mtm: MainThreadMarker, rect: NSRect, style: NSWindowStyleMask) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PanelIvars { key_ok: Cell::new(false) });
        unsafe { msg_send![super(this), initWithContentRect: rect, styleMask: style, backing: NSBackingStoreType::Buffered, defer: false] }
    }

    pub fn set_key_ok(&self, on: bool) {
        self.ivars().key_ok.set(on);
    }
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
            let p = self.global(e);
            app::with(|a| a.on_mouse_down(self.idx(), p, mods(e), e.clickCount()));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, e: &NSEvent) {
            let p = self.global(e);
            if let Some(Some(out)) = app::with(|a| a.on_mouse_dragged(self.idx(), p)) {
                self.begin_drag(out, e);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, e: &NSEvent) {
            let p = self.global(e);
            if let Some(Some(app::Deferred::Share { items, anchor })) = app::with(|a| a.on_mouse_up(self.idx(), p)) {
                self.share(items, anchor);
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, e: &NSEvent) {
            let p = self.global(e);
            if let Some(Some(req)) = app::with(|a| a.on_right_down(self.idx(), p)) {
                self.context_menu(req, e);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, e: &NSEvent) {
            let (dx, dy) = (e.scrollingDeltaX(), e.scrollingDeltaY());
            let precise = e.hasPreciseScrollingDeltas();
            app::with(|a| a.on_scroll(self.idx(), dx, dy, precise));
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
            app::with(|a| a.on_drag_hover(self.idx(), false));
        }

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag(&self, _info: &AnyObject) -> bool {
            true
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, info: &AnyObject) -> bool {
            let pb: Retained<AnyObject> = unsafe { msg_send![info, draggingPasteboard] };
            app::with(|a| a.on_drop(self.idx(), &pb)).unwrap_or(false)
        }

        #[unsafe(method(concludeDragOperation:))]
        fn conclude_drag(&self, _info: Option<&AnyObject>) {
            app::with(|a| a.on_drag_hover(self.idx(), false));
        }

        // NSDraggingSource

        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_mask(&self, _s: &AnyObject, ctx: isize) -> usize {
            // Outside the app: copy / link / generic. Never onto ourselves.
            if ctx == 0 {
                1 | 2 | 4
            } else {
                0
            }
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn drag_ended(&self, _s: &AnyObject, _p: NSPoint, op: usize) {
            app::with(|a| a.on_drag_out_ended(op));
        }

        #[unsafe(method(ignoreModifierKeysForDraggingSession:))]
        fn ignore_modifiers(&self, _s: &AnyObject) -> bool {
            false
        }

        // Menus / sharing / display link

        #[unsafe(method(menuAction:))]
        fn menu_action(&self, item: &AnyObject) {
            let tag: isize = unsafe { msg_send![item, tag] };
            let share = app::with(|a| a.on_menu(tag)).flatten();
            if let Some(app::Deferred::Share { items, anchor }) = share {
                self.share(items, anchor);
            }
        }

        #[unsafe(method(sharingServicePicker:didChooseSharingService:))]
        fn picker_chose(&self, _p: &AnyObject, _s: Option<&AnyObject>) {
            app::with(|a| a.set_sharing(false));
        }

        // Note editor (NSTextView delegate)

        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, _n: &AnyObject) {
            app::with(|a| a.on_note_changed());
        }

        #[unsafe(method(textView:doCommandBySelector:))]
        fn text_command(&self, _tv: &AnyObject, cmd: Sel) -> bool {
            let cancel = cmd == sel!(cancelOperation:);
            if cancel {
                app::with(|a| a.end_note_edit());
            }
            cancel
        }

        #[unsafe(method(displayTick:))]
        fn display_tick(&self, _link: &AnyObject) {
            log::trace!("display tick");
            app::tick(true);
        }
    }
);

fn mods(e: &NSEvent) -> Mods {
    let f = e.modifierFlags();
    Mods { cmd: f.contains(NSEventModifierFlags::Command), shift: f.contains(NSEventModifierFlags::Shift) }
}

impl ShelfView {
    pub fn new(mtm: MainThreadMarker, frame: NSRect, panel: usize) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { panel: Cell::new(panel) });
        let v: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let types: Vec<_> = pboard::ACCEPTED.iter().map(|t| ns(t)).collect();
        let arr = NSArray::from_retained_slice(&types);
        unsafe {
            let _: () = msg_send![&*v, registerForDraggedTypes: &*arr];
        }
        v
    }

    fn idx(&self) -> usize {
        self.ivars().panel.get()
    }

    /// Global top-left point of an event.
    fn global(&self, e: &NSEvent) -> Point {
        let lw = e.locationInWindow();
        let local: NSPoint = unsafe { msg_send![self, convertPoint: lw, fromView: std::ptr::null::<AnyObject>()] };
        // The view is not flipped (so AppKit leaves our y-up layer tree
        // alone); app coordinates are top-left.
        let h = self.bounds().size.height;
        app::view_to_global(self.idx(), Point::new(local.x, h - local.y))
    }

    /// Top-left view rect to AppKit's y-up view coordinates.
    fn to_view(&self, r: crate::geometry::Rect) -> NSRect {
        let h = self.bounds().size.height;
        nsrect(crate::geometry::Rect::new(r.x, h - (r.y + r.h), r.w, r.h))
    }

    fn drag_update(&self, info: &AnyObject) -> usize {
        let source: Option<Retained<AnyObject>> = unsafe { msg_send![info, draggingSource] };
        let own = source.is_some_and(|s| std::ptr::eq(&*s as *const AnyObject, self as *const Self as *const AnyObject));
        if own {
            return 0;
        }
        let pb: Retained<AnyObject> = unsafe { msg_send![info, draggingPasteboard] };
        let ok = pboard::accepts(&pboard::types_of(&pb));
        app::with(|a| a.on_drag_hover(self.idx(), ok));
        if ok {
            1 // NSDragOperationCopy
        } else {
            0
        }
    }

    fn begin_drag(&self, out: app::DragOut, e: &NSEvent) {
        let mut items = vec![];
        for (writer, frame, image) in out.items {
            unsafe {
                let a: objc2::rc::Allocated<AnyObject> = msg_send![objc2::class!(NSDraggingItem), alloc];
                let di: Retained<AnyObject> = msg_send![a, initWithPasteboardWriter: &*writer];
                let _: () = msg_send![&*di, setDraggingFrame: self.to_view(frame), contents: &*image];
                items.push(di);
            }
        }
        if items.is_empty() {
            app::with(|a| a.on_drag_out_ended(0));
            return;
        }
        let n = items.len();
        let arr = NSArray::from_retained_slice(&items);
        unsafe {
            let session: Retained<AnyObject> = msg_send![self, beginDraggingSessionWithItems: &*arr, event: e, source: self];
            let _: () = msg_send![&*session, setAnimatesToStartingPositionsOnCancelOrFail: true];
            if n > 1 {
                // NSDraggingFormationStack
                let _: () = msg_send![&*session, setDraggingFormation: 2isize];
            }
        }
    }

    fn share(&self, items: Vec<Retained<AnyObject>>, anchor: crate::geometry::Rect) {
        if items.is_empty() {
            return;
        }
        let arr = NSArray::from_retained_slice(&items);
        unsafe {
            let a: objc2::rc::Allocated<AnyObject> = msg_send![objc2::class!(NSSharingServicePicker), alloc];
            let picker: Retained<AnyObject> = msg_send![a, initWithItems: &*arr];
            let _: () = msg_send![&*picker, setDelegate: self];
            app::with(|a| a.keep_picker(picker.clone()));
            // NSMinYEdge (1): below the anchor.
            let _: () = msg_send![&*picker, showRelativeToRect: self.to_view(anchor), ofView: self, preferredEdge: 1usize];
        }
    }

    fn context_menu(&self, req: app::MenuReq, e: &NSEvent) {
        let mtm = MainThreadMarker::from(self);
        let _ = mtm;
        unsafe {
            let menu: Retained<AnyObject> = msg_send![objc2::class!(NSMenu), new];
            let _: () = msg_send![&*menu, setAutoenablesItems: false];
            for (title, tag, symbol, enabled) in req.entries {
                if tag < 0 {
                    let sep: Retained<AnyObject> = msg_send![objc2::class!(NSMenuItem), separatorItem];
                    let _: () = msg_send![&*menu, addItem: &*sep];
                    continue;
                }
                let item: Retained<AnyObject> = msg_send![&*menu, addItemWithTitle: &*ns(&title), action: sel!(menuAction:), keyEquivalent: &*ns("")];
                let _: () = msg_send![&*item, setTarget: self];
                let _: () = msg_send![&*item, setTag: tag];
                let _: () = msg_send![&*item, setEnabled: enabled];
                if let Some(img) = super::gfx::symbol_nsimage(symbol, 13.0, super::gfx::W_REGULAR) {
                    let _: () = msg_send![&*item, setImage: &*img];
                }
            }
            app::with(|a| a.set_menu_open(true));
            let lw = e.locationInWindow();
            let local: NSPoint = msg_send![self, convertPoint: lw, fromView: std::ptr::null::<AnyObject>()];
            let _: bool = msg_send![&*menu, popUpMenuPositioningItem: std::ptr::null::<AnyObject>(), atLocation: local, inView: self];
            app::with(|a| a.set_menu_open(false));
        }
    }
}
