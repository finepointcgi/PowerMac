//! [`Awake`]: thread-safe controller owning a dedicated worker thread on which
//! the power backend lives (required for Windows' per-thread execution state).

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::engine::{Clock, Engine, PowerBackend, ProcessProbe, SystemClock};
use crate::mode::{AwakeConfig, AwakeError, AwakeStatus};
use crate::platform::{NativePowerBackend, NativeProcessProbe};

enum Cmd {
    Set(AwakeConfig, Sender<Result<(), AwakeError>>),
    Stop(Sender<()>),
    Status(Sender<AwakeStatus>),
    Shutdown,
}

/// Keeps the machine awake according to an [`AwakeConfig`].
///
/// All methods are `&self` and the type is `Send + Sync`, so it can be shared
/// (e.g. in an `Arc`). Dropping it releases any held assertion and joins the
/// worker thread.
pub struct Awake {
    tx: Sender<Cmd>,
    worker: Option<JoinHandle<()>>,
}

impl Awake {
    /// Controller using the native backend for this OS and the system clock.
    pub fn new() -> Result<Self, AwakeError> {
        Self::with_parts(|| Ok((NativePowerBackend::new(), NativeProcessProbe::default())), SystemClock)
    }

    /// Controller with a custom backend/probe (constructed *on the worker
    /// thread* by `make`) and clock. Used for tests and embedding.
    pub fn with_parts<B, P, F, C>(make: F, clock: C) -> Result<Self, AwakeError>
    where
        B: PowerBackend + 'static,
        P: ProcessProbe + 'static,
        F: FnOnce() -> Result<(B, P), AwakeError> + Send + 'static,
        C: Clock,
    {
        Self::with_parts_poll(make, clock, Duration::from_secs(1))
    }

    /// Like [`Awake::with_parts`] with a custom pid poll interval.
    pub fn with_parts_poll<B, P, F, C>(
        make: F,
        clock: C,
        pid_poll_interval: Duration,
    ) -> Result<Self, AwakeError>
    where
        B: PowerBackend + 'static,
        P: ProcessProbe + 'static,
        F: FnOnce() -> Result<(B, P), AwakeError> + Send + 'static,
        C: Clock,
    {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), AwakeError>>();
        let worker = std::thread::Builder::new()
            .name("awake".into())
            .spawn(move || match make() {
                Ok((backend, probe)) => {
                    let _ = ready_tx.send(Ok(()));
                    let mut engine = Engine::new(backend, probe);
                    engine.pid_poll_interval = pid_poll_interval;
                    run(engine, clock, rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| AwakeError::Backend(format!("spawn awake thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { tx, worker: Some(worker) }),
            Ok(Err(e)) => {
                let _ = worker.join();
                Err(e)
            }
            Err(_) => {
                let _ = worker.join();
                Err(AwakeError::WorkerGone)
            }
        }
    }

    /// Apply `config`, replacing any previous mode. Passive == `stop()`.
    pub fn set(&self, config: AwakeConfig) -> Result<(), AwakeError> {
        let (rtx, rrx) = mpsc::channel();
        self.tx.send(Cmd::Set(config, rtx)).map_err(|_| AwakeError::WorkerGone)?;
        rrx.recv().map_err(|_| AwakeError::WorkerGone)?
    }

    /// Current state (mode, remaining time, ...). Expiry is evaluated first,
    /// so a just-elapsed timer is reported as passive.
    pub fn status(&self) -> AwakeStatus {
        let (rtx, rrx) = mpsc::channel();
        if self.tx.send(Cmd::Status(rtx)).is_err() {
            return AwakeStatus::default();
        }
        rrx.recv().unwrap_or_default()
    }

    /// Release any assertion and go passive.
    pub fn stop(&self) {
        let (rtx, rrx) = mpsc::channel();
        if self.tx.send(Cmd::Stop(rtx)).is_ok() {
            let _ = rrx.recv();
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn run<B: PowerBackend, P: ProcessProbe, C: Clock>(
    mut engine: Engine<B, P>,
    clock: C,
    rx: Receiver<Cmd>,
) {
    loop {
        let cmd = match engine.next_wake(clock.now()) {
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            },
        };
        engine.tick(clock.now());
        match cmd {
            None => {}
            Some(Cmd::Set(cfg, reply)) => {
                let _ = reply.send(engine.set(cfg, clock.now()));
            }
            Some(Cmd::Stop(reply)) => {
                engine.stop();
                let _ = reply.send(());
            }
            Some(Cmd::Status(reply)) => {
                let _ = reply.send(engine.status(clock.now()));
            }
            Some(Cmd::Shutdown) => break,
        }
    }
    engine.stop();
}
