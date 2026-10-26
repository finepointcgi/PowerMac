//! Bridges the system clipboard and Mouse Without Borders: text copied here is
//! sent to peers, and text received from peers is put on the local clipboard.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::RUNNING;

pub fn start(events: Option<Receiver<mwb::EngineEvent>>, share: bool) {
    // Last text we saw or set, so a received clipboard isn't echoed back.
    let last = Arc::new(Mutex::new(String::new()));

    if let Some(rx) = events {
        let last = last.clone();
        std::thread::Builder::new()
            .name("mwb-events".into())
            .spawn(move || {
                let mut cb = arboard::Clipboard::new().ok();
                for ev in rx {
                    match ev {
                        mwb::EngineEvent::ClipboardText { from, text } if share => {
                            log::info!("mwb: clipboard text from {from} ({} chars)", text.chars().count());
                            *last.lock().unwrap() = text.clone();
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

    if !share {
        return;
    }
    std::thread::Builder::new()
        .name("clipboard-watch".into())
        .spawn(move || {
            let Ok(mut cb) = arboard::Clipboard::new() else {
                log::warn!("clipboard: unavailable, not sharing");
                return;
            };
            // Don't push whatever was on the clipboard before we started.
            *last.lock().unwrap() = cb.get_text().unwrap_or_default();
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let Ok(text) = cb.get_text() else { continue };
                {
                    let mut l = last.lock().unwrap();
                    if text.is_empty() || *l == text {
                        continue;
                    }
                    *l = text.clone();
                }
                let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
                let Some(m) = guard.as_ref().and_then(|r| r.mwb.as_ref()) else { return };
                if let Err(e) = m.send_clipboard_text(&text) {
                    log::warn!("clipboard: {e:#}");
                }
            }
        })
        .expect("spawn clipboard-watch");
}
