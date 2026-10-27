//! Moving work off the tap thread.

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

type Job = Box<dyn FnOnce() + Send>;

/// A thread that runs closures in order. Handlers [`Worker::submit`] slow
/// work (Accessibility calls, I/O, launching apps) and return at once.
/// Dropping the worker finishes the queued jobs and joins the thread.
pub struct Worker {
    tx: Mutex<Option<Sender<Job>>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn new(name: &str) -> std::io::Result<Worker> {
        let (tx, rx) = mpsc::channel::<Job>();
        let thread = std::thread::Builder::new().name(name.to_string()).spawn(move || {
            for job in rx {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                    log::error!("pm-hook: a worker job panicked");
                }
            }
        })?;
        Ok(Worker { tx: Mutex::new(Some(tx)), thread: Some(thread) })
    }

    /// Queue `f`; false when the worker is shutting down.
    pub fn submit(&self, f: impl FnOnce() + Send + 'static) -> bool {
        self.push(Box::new(f)).is_ok()
    }

    fn push(&self, job: Job) -> Result<(), Job> {
        let g = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            Some(tx) => tx.send(job).map_err(|e| e.0),
            None => Err(job),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(t) = self.thread.take() {
            if t.thread().id() != std::thread::current().id() {
                let _ = t.join();
            }
        }
    }
}

/// Run `f` on a shared background thread (`pm-hook-defer`). For occasional
/// work; a module with a steady stream of jobs should own a [`Worker`].
pub fn defer(f: impl FnOnce() + Send + 'static) {
    static SHARED: OnceLock<Option<Worker>> = OnceLock::new();
    let job: Job = Box::new(f);
    let job = match SHARED.get_or_init(|| Worker::new("pm-hook-defer").ok()) {
        Some(w) => match w.push(job) {
            Ok(()) => return,
            Err(j) => j,
        },
        None => job,
    };
    let _ = std::thread::Builder::new().name("pm-hook-defer".into()).spawn(job);
}
