//! Bridges the system clipboard and Mouse Without Borders: text copied here is
//! sent to peers, and text received from peers is put on the local clipboard.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::time::Duration;

use crate::RUNNING;

/// Whether clipboard sharing is currently on (MWB enabled + `shareClipboard`).
pub static SHARE: AtomicBool = AtomicBool::new(false);

/// Last text we saw or set, so a received clipboard isn't echoed back.
static LAST: Mutex<String> = Mutex::new(String::new());

/// Put clipboard text received from a peer on the local clipboard. `cb` is
/// opened on first use. Returns whether `ev` was a clipboard event.
pub fn on_event(cb: &mut Option<arboard::Clipboard>, ev: &mwb::EngineEvent) -> bool {
    let mwb::EngineEvent::ClipboardText { from, text } = ev else { return false };
    if !SHARE.load(Ordering::Relaxed) {
        return true;
    }
    log::info!("mwb: clipboard text from {from} ({} chars)", text.chars().count());
    *LAST.lock().unwrap() = text.clone();
    if cb.is_none() {
        *cb = arboard::Clipboard::new().ok();
    }
    if let Some(cb) = cb.as_mut() {
        if let Err(e) = cb.set_text(text.clone()) {
            log::warn!("clipboard: {e}");
        }
    }
    true
}

/// One thread for the life of the process: polls the clipboard and sends new
/// text to peers while sharing is on.
pub fn start_watch_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::thread::Builder::new()
            .name("clipboard-watch".into())
            .spawn(|| {
                let Ok(mut cb) = arboard::Clipboard::new() else {
                    log::warn!("clipboard: unavailable, not sharing");
                    return;
                };
                // Don't push whatever was on the clipboard before we started.
                *LAST.lock().unwrap() = cb.get_text().unwrap_or_default();
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    if !SHARE.load(Ordering::Relaxed) {
                        continue;
                    }
                    let Ok(text) = cb.get_text() else { continue };
                    {
                        let mut l = LAST.lock().unwrap();
                        if text.is_empty() || *l == text {
                            continue;
                        }
                        *l = text.clone();
                    }
                    let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(m) = guard.as_ref().and_then(|r| r.mwb.as_ref()) {
                        if let Err(e) = m.send_clipboard_text(&text) {
                            log::warn!("clipboard: {e:#}");
                        }
                    }
                }
            })
            .expect("spawn clipboard-watch");
    });
}
