//! Shared pieces of PowerMac's shelves: the [Notch Shelf] and the
//! [Drag Shelf] keep the same items, read drops the same way and look the
//! same, so what they share lives here:
//!
//! * [`store`]: the item model (files, links, text), adding with
//!   de-duplication and copies of temporary files, persistence as JSON.
//! * [`geom`] and [`anim`]: rectangles / paths and the damped spring.
//! * `mac` (macOS only): Core Graphics / Core Animation bindings and layer
//!   helpers ([`mac::gfx`]), and the pasteboard code ([`mac::pboard`]):
//!   reading drops (files, URLs, text, images, file promises), drag-out
//!   writers, Quick Look thumbnails, Open / Show in Finder / Copy, the
//!   share picker (AirDrop) and the haptic tick.
//!
//! [Notch Shelf]: ../notch_shelf/index.html
//! [Drag Shelf]: ../drag_shelf/index.html

pub mod anim;
pub mod geom;
pub mod store;
pub mod uti;

#[cfg(target_os = "macos")]
pub mod mac;
