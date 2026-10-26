//! Bridges the system clipboard and Mouse Without Borders: text copied here is
//! sent to peers, and text received from peers is put on the local clipboard.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Mutex, Once};
use std::time::Duration;

use crate::RUNNING;

/// Whether clipboard sharing is currently on (MWB enabled + `shareClipboard`).
pub static SHARE: AtomicBool = AtomicBool::new(false);

/// Last text we saw or set, so a received clipboard isn't echoed back.
static LAST: Mutex<String> = Mutex::new(String::new());

/// Log engine events and put received clipboard text on the local clipboard.
/// Called once per engine start; the thread ends when that engine stops.
pub fn forward_events(events: Option<Receiver<mwb::EngineEvent>>) {
    start_watch_once();
    let Some(rx) = events else { return };
    std::thread::Builder::new()
        .name("mwb-events".into())
        .spawn(move || {
            let mut cb = arboard::Clipboard::new().ok();
            for ev in rx {
                match ev {
                    mwb::EngineEvent::ClipboardText { from, text } if SHARE.load(Ordering::Relaxed) => {
                        log::info!("mwb: clipboard text from {from} ({} chars)", text.chars().count());
                        *LAST.lock().unwrap() = text.clone();
                        if let Some(cb) = cb.as_mut() {
                            if let Err(e) = cb.set_text(text) {
                                log::warn!("clipboard: {e}");
                            }
                        }
                    }
                    other => log::info!("mwb: {other:?}"),
                }
            }
        })
        .expect("spawn mwb-events");
}

/// One thread for the life of the process: polls the clipboard and sends new
/// text to peers while sharing is on.
fn start_watch_once() {
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
