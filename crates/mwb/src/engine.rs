//! The MWB engine: sockets, handshake, machine pool, edge switching and
//! input forwarding / injection.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use log::{debug, info, warn};
use pm_input::{CaptureHandle, Disposition, InputBackend, InputEvent, KeyEvent, MouseButton, MouseEvent, Rect};

use crate::clipboard;
use crate::config::Config;
use crate::crypto;
use crate::matrix::{Dir, Matrix};
use crate::protocol::{llkhf, ptype, wm, Data, ID_ALL, ID_NONE, MOVE_MOUSE_RELATIVE};
use crate::transport::{SecureReader, SecureWriter};

const JUMP_PIXELS: i32 = 2;
const SKIP_PIXELS: i32 = 1;
const JUMP_THROTTLE: Duration = Duration::from_millis(100);
const RECENT_IDS: usize = 50;

/// Notifications emitted by the engine.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    PeerConnected { name: String, id: u32 },
    PeerDisconnected { name: String, id: u32 },
    /// Input focus moved; `name` is the machine now receiving input.
    ActiveMachineChanged { name: String, id: u32 },
    /// Clipboard text received from a peer (the host app should put it on the clipboard).
    ClipboardText { from: String, text: String },
    MatrixChanged(Matrix),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerStatus {
    pub name: String,
    pub id: u32,
    /// Number of trusted sockets (MWB normally keeps 2 per peer: one each way).
    pub connections: usize,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub machine_name: String,
    pub machine_id: u32,
    /// Machine currently receiving this machine's input (this machine when local).
    pub active_machine: String,
    pub active_machine_id: u32,
    /// Remote machine currently controlling this one (last Mouse package source).
    pub controlled_by: Option<String>,
    pub peers: Vec<PeerStatus>,
    pub matrix: Matrix,
    pub capture_active: bool,
}

struct Conn {
    serial: u64,
    is_client: bool,
    peer_name: Mutex<String>,
    peer_id: AtomicU32,
    writer: Mutex<SecureWriter<TcpStream>>,
    stream: TcpStream,
}

impl Conn {
    fn send(&self, d: &Data, magic: u32) -> io::Result<()> {
        self.writer.lock().unwrap().send(d, magic)
    }
    fn name(&self) -> String {
        self.peer_name.lock().unwrap().clone()
    }
}

struct SwitchLoc {
    x: i32,
    y: i32,
}

struct State {
    matrix: Matrix,
    /// lower-case name -> (name, id)
    pool: HashMap<String, (String, u32)>,
    /// `desMachineID`: the machine receiving input.
    des: u32,
    controlled_by: Option<u32>,
    primary: Rect,
    desktop: Rect,
    // controller side
    anchor: (i32, i32),
    virt: (i32, i32),
    switch_loc: Option<SwitchLoc>,
    last_jump: Instant,
    last_next_machine: Instant,
    cursor_hidden: bool,
    keys_local_down: HashSet<(u32, bool)>,
    keys_injected_down: HashSet<(u32, bool)>,
    recent_ids: [i32; RECENT_IDS],
    recent_idx: usize,
    last_clip_in: Option<(String, Instant)>,
}

enum Out {
    Send { des: u32, data: Data, except: Option<u32> },
    Flush(Sender<()>),
}

struct Shared {
    cfg: Config,
    key: String,
    magic: u32,
    my_id: u32,
    my_name: String,
    backend: Arc<dyn InputBackend>,
    state: Mutex<State>,
    conns: Mutex<Vec<Arc<Conn>>>,
    connecting: Mutex<HashSet<String>>,
    package_id: AtomicI32,
    serial: AtomicU64,
    stop: AtomicBool,
    out: Mutex<Option<Sender<Out>>>,
    events: Mutex<Option<Sender<EngineEvent>>>,
    capture_active: AtomicBool,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

pub struct Engine;

/// Handle to a running engine. Dropping it stops the engine.
pub struct EngineHandle {
    shared: Arc<Shared>,
    capture: Mutex<Option<Box<dyn CaptureHandle>>>,
    events_rx: Mutex<Option<Receiver<EngineEvent>>>,
    local_addr: SocketAddr,
    stopped: AtomicBool,
}

fn universal(px: i32, py: i32, r: Rect) -> (i32, i32) {
    let w = (r.right - r.left).max(1) as i64;
    let h = (r.bottom - r.top).max(1) as i64;
    ((((px - r.left) as i64) * 65535 / w) as i32, (((py - r.top) as i64) * 65535 / h) as i32)
}

/// `SendMouse` / `MoveMouseEx`: 0..65535 over the desktop bounds -> pixel.
fn from_universal(x: i32, y: i32, r: Rect) -> (i32, i32) {
    let w = (r.right - r.left) as i64;
    let h = (r.bottom - r.top) as i64;
    let px = r.left as i64 + (x as i64) * w / 65535;
    let py = r.top as i64 + (y as i64) * h / 65535;
    (
        (px as i32).clamp(r.left, (r.right - 1).max(r.left)),
        (py as i32).clamp(r.top, (r.bottom - 1).max(r.top)),
    )
}

fn center(r: Rect) -> (i32, i32) {
    (r.left + (r.right - r.left) / 2, r.top + (r.bottom - r.top) / 2)
}

fn edge_dir(x: i32, y: i32, r: Rect) -> Option<Dir> {
    if x < r.left + SKIP_PIXELS {
        Some(Dir::Left)
    } else if x >= r.right - SKIP_PIXELS {
        Some(Dir::Right)
    } else if y < r.top + SKIP_PIXELS {
        Some(Dir::Up)
    } else if y >= r.bottom - SKIP_PIXELS {
        Some(Dir::Down)
    } else {
        None
    }
}

/// Entry point of the cursor on the destination, as `(point, bounds)` to normalize over.
fn jump_point(dir: Dir, x: i32, y: i32, r: Rect) -> (i32, i32) {
    let p = match dir {
        Dir::Right => (r.left + JUMP_PIXELS, y),
        Dir::Left => (r.right - JUMP_PIXELS, y),
        Dir::Up => (x, r.bottom - JUMP_PIXELS),
        Dir::Down => (x, r.top + JUMP_PIXELS),
    };
    universal(p.0, p.1, r)
}

fn tick_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Engine {
    /// Start the engine: listen on `port + 1`, connect to the matrix machines and
    /// install input capture on `backend`.
    pub fn start(config: Config, backend: Arc<dyn InputBackend>) -> anyhow::Result<EngineHandle> {
        let key = crypto::normalize_key(&config.key);
        if !crypto::is_key_valid(&key) {
            bail!("security key must have at least 16 characters");
        }
        let magic = crypto::get_24bit_hash(&key);
        if magic & 0x8000_0000 != 0 {
            warn!("this key's magic number has bit 31 set; Windows MWB rejects every package for such keys");
        }
        let my_name = config.effective_machine_name();
        let my_id = config.machine_id.unwrap_or_else(|| loop {
            let v = u32::from_le_bytes(crypto::random_bytes::<4>()) & 0x7FFF_FFFF;
            if v != ID_NONE && v != ID_ALL {
                break v;
            }
        });
        let mut matrix = config.matrix();
        if !matrix.contains(&my_name) {
            if let Some(slot) = matrix.slots.iter_mut().find(|s| s.trim().is_empty()) {
                *slot = my_name.clone();
            }
        }
        let screens = backend.screens().unwrap_or_default();
        let primary = screens.first().copied().unwrap_or(Rect { left: 0, top: 0, right: 1920, bottom: 1080 });
        let desktop = backend.desktop_bounds().unwrap_or(primary);

        let listener = TcpListener::bind((config.bind_address.as_str(), config.port.wrapping_add(1)))
            .with_context(|| format!("binding {}:{}", config.bind_address, config.port as u32 + 1))?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;

        let mut pool = HashMap::new();
        pool.insert(my_name.to_lowercase(), (my_name.clone(), my_id));
        let (out_tx, out_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let start_id = (u32::from_le_bytes(crypto::random_bytes::<4>()) & 0x3FFF_FFFF) as i32 + 1;
        let now = Instant::now();
        let shared = Arc::new(Shared {
            key,
            magic,
            my_id,
            my_name: my_name.clone(),
            backend: backend.clone(),
            state: Mutex::new(State {
                matrix,
                pool,
                des: my_id,
                controlled_by: None,
                primary,
                desktop,
                anchor: center(primary),
                virt: (0, 0),
                switch_loc: None,
                last_jump: now - Duration::from_secs(1),
                last_next_machine: now - Duration::from_secs(1),
                cursor_hidden: false,
                keys_local_down: HashSet::new(),
                keys_injected_down: HashSet::new(),
                recent_ids: [0; RECENT_IDS],
                recent_idx: 0,
                last_clip_in: None,
            }),
            conns: Mutex::new(Vec::new()),
            connecting: Mutex::new(HashSet::new()),
            package_id: AtomicI32::new(start_id),
            serial: AtomicU64::new(1),
            stop: AtomicBool::new(false),
            out: Mutex::new(Some(out_tx)),
            events: Mutex::new(Some(ev_tx)),
            capture_active: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
            cfg: config,
        });
        info!("MWB engine '{}' id {} listening on {}", my_name, my_id, local_addr);

        // Sender thread.
        {
            let s = shared.clone();
            let h = thread::Builder::new().name("mwb-send".into()).spawn(move || s.sender_loop(out_rx))?;
            shared.threads.lock().unwrap().push(h);
        }
        // Listener thread.
        {
            let s = shared.clone();
            let h = thread::Builder::new().name("mwb-listen".into()).spawn(move || s.listen_loop(listener))?;
            shared.threads.lock().unwrap().push(h);
        }
        // Maintenance thread: connect to peers, heartbeats, screen refresh.
        {
            let s = shared.clone();
            let h = thread::Builder::new().name("mwb-maint".into()).spawn(move || s.maintenance_loop())?;
            shared.threads.lock().unwrap().push(h);
        }

        // Input capture.
        let weak = Arc::downgrade(&shared);
        let cb: pm_input::CaptureCallback = Arc::new(move |ev: &InputEvent| match weak.upgrade() {
            Some(s) => s.on_local_input(ev),
            None => Disposition::Pass,
        });
        let capture = match backend.start_capture(cb) {
            Ok(h) => {
                shared.capture_active.store(true, Ordering::SeqCst);
                Some(h)
            }
            Err(e) => {
                warn!("input capture unavailable ({e:#}); this machine can only be controlled");
                None
            }
        };

        Ok(EngineHandle {
            shared,
            capture: Mutex::new(capture),
            events_rx: Mutex::new(Some(ev_rx)),
            local_addr,
            stopped: AtomicBool::new(false),
        })
    }
}

impl EngineHandle {
    pub fn machine_id(&self) -> u32 {
        self.shared.my_id
    }
    pub fn machine_name(&self) -> &str {
        &self.shared.my_name
    }
    /// Address of the message socket (`port + 1`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn status(&self) -> Status {
        self.shared.status()
    }

    /// Names of peers with at least one trusted connection.
    pub fn connected_peers(&self) -> Vec<String> {
        self.status().peers.into_iter().map(|p| p.name).collect()
    }

    /// Name of the machine currently receiving input.
    pub fn active_machine(&self) -> String {
        self.status().active_machine
    }

    /// Take the event receiver (only once).
    pub fn events(&self) -> Option<Receiver<EngineEvent>> {
        self.events_rx.lock().unwrap().take()
    }

    /// Switch input to `name` (MWB `SwitchToMachine`, e.g. from a hotkey/menu).
    pub fn switch_to(&self, name: &str) -> bool {
        self.shared.switch_to_machine(name)
    }

    /// Push clipboard text to all peers (if `share_clipboard`).
    pub fn send_clipboard_text(&self, text: &str) -> anyhow::Result<()> {
        if !self.shared.cfg.share_clipboard {
            return Ok(());
        }
        let payload = clipboard::encode_text(text);
        if payload.len() > clipboard::MAX_INSTANT_SIZE {
            bail!("clipboard text too large for the message socket (big-clipboard transfer not implemented)");
        }
        for d in clipboard::chunk_packages(&payload, false) {
            self.shared.enqueue_except(ID_ALL, d, Some(self.shared.my_id));
        }
        Ok(())
    }

    /// Stop: say ByeBye, close sockets, remove hooks, restore the cursor.
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        let s = &self.shared;
        s.enqueue(ID_ALL, Data::new(ptype::ByeBye));
        s.flush();
        s.stop.store(true, Ordering::SeqCst);
        drop(self.capture.lock().unwrap().take());
        {
            let mut st = s.state.lock().unwrap();
            if st.des != s.my_id {
                let _ = s.backend.set_cursor_locked(false);
            }
            if st.cursor_hidden {
                let _ = s.backend.set_cursor_visible(true);
                st.cursor_hidden = false;
            }
        }
        for c in s.conns.lock().unwrap().iter() {
            let _ = c.stream.shutdown(Shutdown::Both);
        }
        *s.out.lock().unwrap() = None;
        let threads: Vec<_> = s.threads.lock().unwrap().drain(..).collect();
        for t in threads {
            let _ = t.join();
        }
        // connection threads are detached; they exit on socket shutdown.
        for c in s.conns.lock().unwrap().drain(..) {
            let _ = c.stream.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Shared {
    fn emit(&self, e: EngineEvent) {
        if let Some(tx) = self.events.lock().unwrap().as_ref() {
            let _ = tx.send(e);
        }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn enqueue(&self, des: u32, data: Data) {
        self.enqueue_except(des, data, None)
    }

    fn enqueue_except(&self, des: u32, data: Data, except: Option<u32>) {
        if let Some(tx) = self.out.lock().unwrap().as_ref() {
            let _ = tx.send(Out::Send { des, data, except });
        }
    }

    fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        if let Some(o) = self.out.lock().unwrap().as_ref() {
            let _ = o.send(Out::Flush(tx));
        } else {
            return;
        }
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }

    fn package(&self, t: u32) -> Data {
        let mut d = Data::new(t);
        d.set_machine_name(&self.my_name);
        d
    }

    fn sender_loop(self: Arc<Self>, rx: Receiver<Out>) {
        while let Ok(m) = rx.recv() {
            match m {
                Out::Flush(done) => {
                    let _ = done.send(());
                }
                Out::Send { des, mut data, except } => {
                    // Common.SkSend
                    let id = self.package_id.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
                    data.set_id(id);
                    if data.src() == ID_NONE {
                        data.set_src(self.my_id);
                    }
                    data.set_des(des);
                    let matrix = self.st().matrix.clone();
                    let targets: Vec<Arc<Conn>> = self
                        .conns
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|c| {
                            let pid = c.peer_id.load(Ordering::SeqCst);
                            pid == des
                                || (des == ID_ALL && Some(pid) != except && matrix.contains(&c.name()))
                        })
                        .cloned()
                        .collect();
                    for c in targets {
                        if let Err(e) = c.send(&data, self.magic) {
                            debug!("send to {} failed: {e}", c.name());
                            let _ = c.stream.shutdown(Shutdown::Both);
                        }
                    }
                }
            }
        }
    }

    fn listen_loop(self: Arc<Self>, listener: TcpListener) {
        while !self.stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, addr)) => {
                    debug!("accepted connection from {addr}");
                    let _ = stream.set_nonblocking(false);
                    let s = self.clone();
                    let _ = thread::Builder::new()
                        .name(format!("mwb-srv-{addr}"))
                        .spawn(move || s.run_connection(stream, false, addr.ip().to_string()));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    warn!("accept failed: {e}");
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    fn maintenance_loop(self: Arc<Self>) {
        let mut last_hb = Instant::now();
        let mut last_connect = Instant::now() - Duration::from_secs(3600);
        while !self.stop.load(Ordering::SeqCst) {
            if last_connect.elapsed() >= Duration::from_secs(self.cfg.reconnect_secs.max(1)) {
                last_connect = Instant::now();
                self.update_tcp_clients();
                if let Ok(screens) = self.backend.screens() {
                    if let Some(p) = screens.first().copied() {
                        let d = self.backend.desktop_bounds().unwrap_or(p);
                        let mut st = self.st();
                        st.primary = p;
                        st.desktop = d;
                    }
                }
            }
            if last_hb.elapsed() >= Duration::from_secs(self.cfg.heartbeat_secs.max(1)) {
                last_hb = Instant::now();
                self.enqueue(ID_ALL, self.package(ptype::Heartbeat));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// `SocketStuff.UpdateTCPClients`: make sure we have a client socket to every matrix machine.
    fn update_tcp_clients(self: &Arc<Self>) {
        let names: Vec<String> = self.st().matrix.names().map(|s| s.to_string()).collect();
        for name in names {
            if name.eq_ignore_ascii_case(&self.my_name) {
                continue;
            }
            if self.has_client_conn(&name) {
                continue;
            }
            {
                let mut c = self.connecting.lock().unwrap();
                if !c.insert(name.to_lowercase()) {
                    continue;
                }
            }
            let s = self.clone();
            let _ = thread::Builder::new().name(format!("mwb-cli-{name}")).spawn(move || {
                let r = s.connect_peer(&name);
                s.connecting.lock().unwrap().remove(&name.to_lowercase());
                if let Err(e) = r {
                    debug!("connect to {name}: {e:#}");
                }
            });
        }
    }

    fn has_client_conn(&self, name: &str) -> bool {
        self.conns.lock().unwrap().iter().any(|c| c.is_client && c.name().eq_ignore_ascii_case(name))
    }

    fn resolve_peer_addrs(&self, name: &str) -> anyhow::Result<Vec<SocketAddr>> {
        let spec = self
            .cfg
            .peers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| name.to_string());
        let (host, base) = match spec.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') || h.starts_with('[') => {
                (h.trim_matches(|c| c == '[' || c == ']').to_string(), p.parse::<u16>().context("bad port")?)
            }
            _ => (spec.clone(), self.cfg.port),
        };
        Ok((host.as_str(), base.wrapping_add(1)).to_socket_addrs()?.collect())
    }

    fn connect_peer(self: &Arc<Self>, name: &str) -> anyhow::Result<()> {
        let addrs = self.resolve_peer_addrs(name)?;
        let mut last_err = None;
        for a in addrs {
            match TcpStream::connect_timeout(&a, Duration::from_secs(3)) {
                Ok(s) => {
                    debug!("connected to {name} at {a}");
                    self.clone().run_connection(s, true, name.to_string());
                    return Ok(());
                }
                Err(e) => last_err = Some(e),
            }
        }
        match last_err {
            Some(e) => Err(e.into()),
            None => bail!("no address for {name}"),
        }
    }

    /// `SocketStuff.MainTCPRoutine`.
    fn run_connection(self: Arc<Self>, stream: TcpStream, is_client: bool, hint_name: String) {
        if let Err(e) = self.clone().connection_inner(stream, is_client, hint_name.clone()) {
            debug!("connection {hint_name} ended: {e}");
        }
    }

    fn connection_inner(self: Arc<Self>, stream: TcpStream, is_client: bool, hint_name: String) -> io::Result<()> {
        if self.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut writer = SecureWriter::new(stream.try_clone()?, &self.key)?;

        // Handshake challenge: random 64 bytes, our name, type Handshake; sent 10 times.
        let mut d = Data::from_bytes(&crypto::random_bytes::<64>());
        d.set_machine_name(&self.my_name);
        d.set_type(ptype::Handshake);
        if d.src() == ID_NONE {
            d.set_src(self.my_id);
        }
        for _ in 0..10 {
            writer.send(&d, self.magic)?;
        }
        let expect: [u32; 4] = [!d.machine(0), !d.machine(1), !d.machine(2), !d.machine(3)];

        let mut reader = SecureReader::new(stream.try_clone()?, &self.key)?;
        let mut package_count: i32 = 0;
        let mut writer = Some(writer);
        let mut conn: Option<Arc<Conn>> = None;
        let mut clip_acc: Option<(bool, Vec<u8>)> = None;

        let result = loop {
            if self.stop.load(Ordering::SeqCst) {
                break Ok(());
            }
            let pkt = match reader.recv(self.magic) {
                Ok(p) => p,
                Err(e) => break Err(e),
            };
            let mut p = pkt.data;
            let t = p.ptype();
            if t == ptype::Handshake {
                p.set_type(ptype::HandshakeAck);
                p.set_src(self.my_id);
                p.set_machine_name(&self.my_name);
                for i in 0..4 {
                    p.set_machine(i, !p.machine(i));
                }
                let r = match (&mut writer, &conn) {
                    (Some(w), _) => w.send(&p, self.magic),
                    (None, Some(c)) => c.send(&p, self.magic),
                    _ => Ok(()),
                };
                if let Err(e) = r {
                    break Err(e);
                }
                continue;
            }
            if package_count >= 0 {
                package_count += 1;
                if package_count >= 10 {
                    warn!("more than 10 invalid packages from {hint_name}: wrong security key?");
                    if let Some(w) = writer.as_mut() {
                        let mut inv = p.clone();
                        inv.set_type(ptype::Invalid);
                        for _ in 0..10 {
                            let _ = w.send(&inv, self.magic);
                        }
                    }
                    break Err(io::Error::new(io::ErrorKind::PermissionDenied, "invalid key"));
                } else if t == ptype::HandshakeAck {
                    if (0..4).all(|i| p.machine(i) == expect[i]) {
                        let name = p.machine_name();
                        let remote_id = p.src();
                        if name.eq_ignore_ascii_case(&self.my_name) {
                            debug!("connected to ourselves, closing");
                            break Ok(());
                        }
                        if is_client && self.has_client_conn(&name) {
                            debug!("duplicate client socket to {name}");
                            break Ok(());
                        }
                        package_count = -1;
                        let c = Arc::new(Conn {
                            serial: self.serial.fetch_add(1, Ordering::SeqCst),
                            is_client,
                            peer_name: Mutex::new(name.clone()),
                            peer_id: AtomicU32::new(remote_id),
                            writer: Mutex::new(writer.take().unwrap()),
                            stream: stream.try_clone()?,
                        });
                        let _ = stream.set_read_timeout(None);
                        self.conns.lock().unwrap().push(c.clone());
                        conn = Some(c);
                        info!("trusted {} '{}' id {}", if is_client { "client" } else { "server" }, name, remote_id);
                        let first = self.peer_conn_count(remote_id) == 1;
                        self.learn_machine(&name, remote_id);
                        if first {
                            self.emit(EngineEvent::PeerConnected { name: name.clone(), id: remote_id });
                        }
                        // Common.SendHeartBeat(initial: true)
                        self.enqueue(ID_ALL, self.package(ptype::Heartbeat));
                        if !is_client {
                            self.update_tcp_clients();
                        }
                    } else {
                        warn!("invalid handshake ack from {hint_name}");
                        break Err(io::Error::new(io::ErrorKind::PermissionDenied, "invalid ack"));
                    }
                } else if t == ptype::Mouse || t == ptype::Heartbeat || t == ptype::Heartbeat_ex {
                    if package_count > 5 {
                        package_count -= 1;
                    }
                } else {
                    debug!("unexpected package before handshake: type {t}");
                }
                continue;
            }
            // Trusted.
            if t == ptype::HandshakeAck {
                continue;
            }
            let c = conn.as_ref().unwrap().clone();
            // Inline clipboard reception (ReceiveClipboardDataUsingTCP).
            if t == ptype::ClipboardText || t == ptype::ClipboardImage {
                let acc = clip_acc.get_or_insert_with(|| (t == ptype::ClipboardImage, Vec::new()));
                acc.1.extend_from_slice(&p.buf[clipboard::DATA_START..]);
                continue;
            }
            if t == ptype::ClipboardDataEnd {
                if let Some((image, data)) = clip_acc.take() {
                    self.on_clipboard_data(&c, image, &data);
                }
                continue;
            }
            self.process_package(&c, p);
        };

        // Cleanup.
        let _ = stream.shutdown(Shutdown::Both);
        if let Some(c) = conn {
            self.conns.lock().unwrap().retain(|x| x.serial != c.serial);
            let id = c.peer_id.load(Ordering::SeqCst);
            if self.peer_conn_count(id) == 0 {
                let name = c.name();
                info!("peer '{name}' disconnected");
                self.emit(EngineEvent::PeerDisconnected { name, id });
                let back = {
                    let st = self.st();
                    st.des == id
                };
                if back {
                    self.switch_to_local_center();
                }
                let mut st = self.st();
                if st.controlled_by == Some(id) {
                    st.controlled_by = None;
                }
            }
        }
        result
    }

    fn peer_conn_count(&self, id: u32) -> usize {
        self.conns.lock().unwrap().iter().filter(|c| c.peer_id.load(Ordering::SeqCst) == id).count()
    }

    fn is_connected(&self, id: u32) -> bool {
        id == self.my_id || self.peer_conn_count(id) > 0
    }

    fn is_connected_name(&self, st: &State, name: &str) -> bool {
        match st.pool.get(&name.to_lowercase()) {
            Some((_, id)) => self.is_connected(*id),
            None => false,
        }
    }

    fn learn_machine(&self, name: &str, id: u32) {
        if name.trim().is_empty() || id == ID_NONE || id == ID_ALL {
            return;
        }
        let mut st = self.st();
        st.pool.insert(name.to_lowercase(), (name.to_string(), id));
    }

    fn name_of(&self, st: &State, id: u32) -> Option<String> {
        st.pool.values().find(|(_, i)| *i == id).map(|(n, _)| n.clone())
    }

    fn status(&self) -> Status {
        let st = self.st();
        let conns = self.conns.lock().unwrap();
        let mut peers: HashMap<u32, PeerStatus> = HashMap::new();
        for c in conns.iter() {
            let id = c.peer_id.load(Ordering::SeqCst);
            let e = peers.entry(id).or_insert_with(|| PeerStatus { name: c.name(), id, connections: 0 });
            e.connections += 1;
        }
        let mut peers: Vec<_> = peers.into_values().collect();
        peers.sort_by(|a, b| a.name.cmp(&b.name));
        Status {
            machine_name: self.my_name.clone(),
            machine_id: self.my_id,
            active_machine: self.name_of(&st, st.des).unwrap_or_default(),
            active_machine_id: st.des,
            controlled_by: st.controlled_by.and_then(|i| self.name_of(&st, i)),
            peers,
            matrix: st.matrix.clone(),
            capture_active: self.capture_active.load(Ordering::SeqCst),
        }
    }

    fn neighbour(&self, st: &State, current: &str, dir: Dir) -> Option<u32> {
        let resolve = |n: &str| st.pool.get(&n.to_lowercase()).map(|(_, id)| *id);
        let connected = |n: &str| self.is_connected_name(st, n);
        st.matrix.neighbour(current, dir, &resolve, &connected)
    }

    // ------------------------------------------------------------------
    // Local input (capture callback)
    // ------------------------------------------------------------------

    fn on_local_input(&self, ev: &InputEvent) -> Disposition {
        let mut st = self.st();
        match *ev {
            InputEvent::Key(k) => self.on_local_key(&mut st, k),
            InputEvent::Mouse(m) => self.on_local_mouse(&mut st, m),
        }
    }

    fn on_local_key(&self, st: &mut State, k: KeyEvent) -> Disposition {
        let entry = (k.vk, k.extended);
        let local = st.des == self.my_id || st.des == ID_ALL;
        let k = if !local && self.cfg.swap_cmd_ctrl { pm_input::vk::swap_cmd_ctrl_event(k) } else { k };
        if st.des == self.my_id || st.des == ID_ALL {
            if k.down {
                st.keys_local_down.insert(entry);
            } else {
                st.keys_local_down.remove(&entry);
            }
            return Disposition::Pass;
        }
        let alt = st.keys_local_down.iter().any(|(vk, _)| matches!(*vk, 0x12 | 0xA4 | 0xA5));
        let mut flags = 0;
        if !k.down {
            flags |= llkhf::UP;
        }
        if k.extended {
            flags |= llkhf::EXTENDED;
        }
        if alt || (k.vk == 0x12 || k.vk == 0xA4 || k.vk == 0xA5) && k.down {
            flags |= llkhf::ALTDOWN;
        }
        let mut d = Data::new(ptype::Keyboard);
        d.set_date_time(tick_ms());
        d.set_kd_vk(k.vk as i32);
        d.set_kd_flags(flags);
        self.enqueue(st.des, d);
        Disposition::Swallow
    }

    fn on_local_mouse(&self, st: &mut State, m: MouseEvent) -> Disposition {
        let local = st.des == self.my_id || st.des == ID_ALL;
        if local {
            if let MouseEvent::Move { x, y, .. } = m {
                if self.cfg.easy_mouse && st.des == self.my_id {
                    if let Some(dir) = edge_dir(x, y, st.desktop) {
                        if st.last_jump.elapsed() >= JUMP_THROTTLE {
                            if let Some(new_id) = self.neighbour(st, &self.my_name, dir) {
                                if new_id != self.my_id {
                                    let p = jump_point(dir, x, y, st.desktop);
                                    self.prepare_to_switch(st, new_id, p);
                                }
                            }
                        }
                    }
                }
            }
            return Disposition::Pass;
        }

        // Remote: compute the virtual position (InputHook.MouseHookProc).
        let p = st.primary;
        let pw = (p.right - p.left).max(1) as i64;
        let ph = (p.bottom - p.top).max(1) as i64;
        let (x, y) = match m {
            MouseEvent::Move { x, y, .. } | MouseEvent::Button { x, y, .. } => (x, y),
            _ => st.anchor,
        };
        let _ = (x, y);
        let (mut dx, mut dy) = (0, 0);
        let mut relative_encoded = false;
        if let Some(sl) = st.switch_loc.take() {
            st.virt = ((sl.x as i64 * pw / 65535) as i32 + p.left, (sl.y as i64 * ph / 65535) as i32 + p.top);
        } else if let MouseEvent::Move { dx: mdx, dy: mdy, .. } = m {
            // The local cursor is frozen (swallowed on Windows, set_cursor_locked on macOS):
            // drive the virtual cursor from the raw deltas.
            dx = mdx;
            dy = mdy;
            let mut v = (st.virt.0 + dx, st.virt.1 + dy);
            if v.0 < p.left {
                v.0 = p.left - 1;
            } else if v.0 > p.right {
                v.0 = p.right + 1;
            }
            if v.1 < p.top {
                v.1 = p.top - 1;
            } else if v.1 > p.bottom {
                v.1 = p.bottom + 1;
            }
            st.virt = v;
            relative_encoded = true;
        } else {
            relative_encoded = true;
        }

        // Event.MouseEvent: edge detection on the virtual cursor (absolute mode).
        if let MouseEvent::Move { .. } = m {
            if self.cfg.easy_mouse && !self.cfg.move_mouse_relatively && st.last_jump.elapsed() >= JUMP_THROTTLE {
                if let Some(dir) = edge_dir(st.virt.0, st.virt.1, p) {
                    let cur = self.name_of(st, st.des).unwrap_or_default();
                    if let Some(new_id) = self.neighbour(st, &cur, dir) {
                        if new_id != st.des {
                            let jp = jump_point(dir, st.virt.0, st.virt.1, p);
                            self.prepare_to_switch(st, new_id, jp);
                        }
                    }
                }
            }
        }
        if st.des == self.my_id || st.des == ID_ALL || st.switch_loc.is_some() {
            return Disposition::Swallow;
        }

        let (flags, wheel) = match m {
            MouseEvent::Move { .. } => (wm::WM_MOUSEMOVE, 0),
            MouseEvent::Button { button, down, .. } => match (button, down) {
                (MouseButton::Left, true) => (wm::WM_LBUTTONDOWN, 0),
                (MouseButton::Left, false) => (wm::WM_LBUTTONUP, 0),
                (MouseButton::Right, true) => (wm::WM_RBUTTONDOWN, 0),
                (MouseButton::Right, false) => (wm::WM_RBUTTONUP, 0),
                (MouseButton::Middle, true) => (wm::WM_MBUTTONDOWN, 0),
                (MouseButton::Middle, false) => (wm::WM_MBUTTONUP, 0),
                (MouseButton::X1, true) => (wm::WM_XBUTTONDOWN, wm::XBUTTON1),
                (MouseButton::X1, false) => (wm::WM_XBUTTONUP, wm::XBUTTON1),
                (MouseButton::X2, true) => (wm::WM_XBUTTONDOWN, wm::XBUTTON2),
                (MouseButton::X2, false) => (wm::WM_XBUTTONUP, wm::XBUTTON2),
            },
            MouseEvent::Wheel { delta } => (wm::WM_MOUSEWHEEL, delta),
            MouseEvent::HWheel { delta } => (wm::WM_MOUSEHWHEEL, delta),
        };
        let mut d = Data::new(ptype::Mouse);
        d.set_md_flags(flags);
        d.set_md_wheel(wheel);
        if self.cfg.move_mouse_relatively && relative_encoded {
            let ex = if dx < 0 { dx - MOVE_MOUSE_RELATIVE } else { dx + MOVE_MOUSE_RELATIVE };
            let ey = if dy < 0 { dy - MOVE_MOUSE_RELATIVE } else { dy + MOVE_MOUSE_RELATIVE };
            d.set_md_x(ex);
            d.set_md_y(ey);
        } else {
            d.set_md_x(((st.virt.0 - p.left) as i64 * 65535 / pw) as i32);
            d.set_md_y(((st.virt.1 - p.top) as i64 * 65535 / ph) as i32);
        }
        self.enqueue(st.des, d);
        Disposition::Swallow
    }

    /// `Event.PrepareToSwitchToMachine` (+ the helper thread's cursor handling).
    fn prepare_to_switch(&self, st: &mut State, new_id: u32, p: (i32, i32)) {
        if st.last_jump.elapsed() < JUMP_THROTTLE || st.des == ID_ALL {
            return;
        }
        st.last_jump = Instant::now();
        if !self.is_connected(new_id) {
            debug!("no active connection for machine {new_id}, cancel switching");
            return;
        }
        if st.des == new_id {
            return;
        }
        let old = st.des;
        if new_id != ID_ALL && old != self.my_id {
            self.enqueue(old, self.package(ptype::HideMouse));
        }
        st.des = new_id;
        if new_id == self.my_id {
            st.switch_loc = None;
            let (px, py) = from_universal(p.0, p.1, st.desktop);
            let _ = self.backend.set_cursor_locked(false);
            let _ = self.backend.set_cursor_pos(px, py);
            if st.cursor_hidden {
                let _ = self.backend.set_cursor_visible(true);
                st.cursor_hidden = false;
            }
        } else {
            st.switch_loc = Some(SwitchLoc { x: p.0, y: p.1 });
            self.enqueue(new_id, self.package(ptype::MachineSwitched));
            if old == self.my_id {
                // Release keys held locally; their key-up would otherwise go to the remote.
                for (vk, extended) in st.keys_local_down.drain().collect::<Vec<_>>() {
                    let _ = self.backend.inject_key(KeyEvent { vk, down: false, extended });
                }
                // Park the real cursor at the primary centre (away from the clamping
                // outer edges) and freeze it while forwarding.
                st.anchor = center(st.primary);
                let _ = self.backend.set_cursor_pos(st.anchor.0, st.anchor.1);
                let _ = self.backend.set_cursor_locked(true);
                if self.cfg.hide_cursor_when_away && !st.cursor_hidden {
                    let _ = self.backend.set_cursor_visible(false);
                    st.cursor_hidden = true;
                }
            }
        }
        let name = self.name_of(st, new_id).unwrap_or_default();
        info!("switched input to '{name}' ({new_id})");
        self.emit(EngineEvent::ActiveMachineChanged { name, id: new_id });
    }

    fn switch_to_machine(&self, name: &str) -> bool {
        let mut st = self.st();
        let Some((_, id)) = st.pool.get(&name.to_lowercase()).cloned() else { return false };
        if id != self.my_id && !self.is_connected(id) {
            return false;
        }
        st.last_jump = Instant::now() - JUMP_THROTTLE;
        // MWB SwitchToMachine: the cursor goes to the centre of the primary screen.
        let c = center(st.primary);
        let p = universal(c.0, c.1, st.primary);
        self.prepare_to_switch(&mut st, id, p);
        st.des == id
    }

    fn switch_to_local_center(&self) {
        let mut st = self.st();
        if st.des == self.my_id {
            return;
        }
        st.last_jump = Instant::now() - JUMP_THROTTLE;
        let c = center(st.desktop);
        let p = universal(c.0, c.1, st.desktop);
        let my = self.my_id;
        // Bypass the connection check for ourselves.
        self.prepare_to_switch(&mut st, my, p);
    }

    // ------------------------------------------------------------------
    // Remote packages (Receiver.ProcessPackage)
    // ------------------------------------------------------------------

    fn dedup(&self, st: &mut State, p: &Data) -> bool {
        let t = p.ptype();
        if t == ptype::Invalid || t == 0 {
            return false;
        }
        if t != ptype::ClipboardText && t != ptype::ClipboardImage && t != ptype::Handshake && t != ptype::HandshakeAck {
            let id = p.id();
            if st.recent_ids.contains(&id) {
                return false;
            }
            st.recent_idx = (st.recent_idx + 1) % RECENT_IDS;
            let i = st.recent_idx;
            st.recent_ids[i] = id;
        }
        true
    }

    fn process_package(&self, conn: &Arc<Conn>, p: Data) {
        let mut st = self.st();
        if !self.dedup(&mut st, &p) {
            return;
        }
        let t = p.ptype();
        let for_me = p.des() == self.my_id || p.des() == ID_ALL;
        match t {
            ptype::Keyboard if for_me => {
                let flags = p.kd_flags();
                let mut k = KeyEvent { vk: p.kd_vk() as u32 & 0xFFFF, down: flags & llkhf::UP == 0, extended: flags & llkhf::EXTENDED != 0 };
                if self.cfg.swap_cmd_ctrl {
                    k = pm_input::vk::swap_cmd_ctrl_event(k);
                }
                if k.down {
                    st.keys_injected_down.insert((k.vk, k.extended));
                } else {
                    st.keys_injected_down.remove(&(k.vk, k.extended));
                }
                drop(st);
                if let Err(e) = self.backend.inject_key(k) {
                    warn!("inject_key: {e:#}");
                }
            }
            ptype::Mouse if for_me => self.on_remote_mouse(st, &p),
            ptype::NextMachine => {
                if self.cfg.easy_mouse && p.des() == self.my_id {
                    let new_id = p.md_wheel() as u32;
                    self.prepare_to_switch(&mut st, new_id, (p.md_x(), p.md_y()));
                }
            }
            ptype::Heartbeat | ptype::Heartbeat_ex | ptype::Awake => {
                drop(st);
                self.learn_from(conn, &p);
            }
            ptype::Hello => {
                drop(st);
                self.learn_from(conn, &p);
                self.enqueue(ID_ALL, self.package(ptype::Heartbeat));
            }
            ptype::Hi | ptype::Heartbeat_ex_l2 | ptype::Heartbeat_ex_l3 => {}
            ptype::ByeBye => {
                let src = p.src();
                let back = st.des == src;
                if st.controlled_by == Some(src) {
                    st.controlled_by = None;
                }
                drop(st);
                if back {
                    self.switch_to_local_center();
                }
            }
            ptype::HideMouse => {
                // Control moved away from this machine.
                st.controlled_by = None;
                let c = center(st.primary);
                let keys: Vec<_> = st.keys_injected_down.drain().collect();
                drop(st);
                let _ = self.backend.set_cursor_pos(c.0, c.1);
                for (vk, extended) in keys {
                    let _ = self.backend.inject_key(KeyEvent { vk, down: false, extended });
                }
            }
            ptype::MachineSwitched | ptype::Clipboard | ptype::ClipboardAsk | ptype::ClipboardPush => {
                // Big-clipboard / file transfer over the clipboard socket: not implemented.
            }
            _ if t & ptype::Matrix == ptype::Matrix => {
                if !self.cfg.accept_remote_matrix {
                    return;
                }
                let i = p.src();
                if (1..=4).contains(&i) {
                    st.matrix.slots[(i - 1) as usize] = p.machine_name();
                    if i == 4 {
                        st.matrix.circle = t & ptype::MatrixSwapFlag == ptype::MatrixSwapFlag;
                        st.matrix.one_row = t & ptype::MatrixTwoRowFlag != ptype::MatrixTwoRowFlag;
                        let m = st.matrix.clone();
                        drop(st);
                        info!("matrix updated by peer: {:?}", m);
                        self.emit(EngineEvent::MatrixChanged(m));
                    }
                }
            }
            _ => {}
        }
    }

    fn learn_from(&self, conn: &Arc<Conn>, p: &Data) {
        let name = p.machine_name();
        let id = p.src();
        self.learn_machine(&name, id);
        if conn.peer_id.load(Ordering::SeqCst) == ID_NONE {
            conn.peer_id.store(id, Ordering::SeqCst);
        }
    }

    fn on_remote_mouse(&self, mut st: MutexGuard<'_, State>, p: &Data) {
        let src = p.src();
        if st.des != self.my_id {
            // Someone else is now controlling us; we stop controlling anyone.
            st.des = self.my_id;
            st.switch_loc = None;
            let _ = self.backend.set_cursor_locked(false);
            if st.cursor_hidden {
                let _ = self.backend.set_cursor_visible(true);
                st.cursor_hidden = false;
            }
        }
        st.controlled_by = Some(src);
        let desktop = st.desktop;
        let (x, y, wheel, flags) = (p.md_x(), p.md_y(), p.md_wheel(), p.md_flags());
        let relative = x.abs() >= MOVE_MOUSE_RELATIVE && y.abs() >= MOVE_MOUSE_RELATIVE;
        let pos = if relative {
            let (cx, cy) = self.backend.cursor_pos().unwrap_or(center(desktop));
            if flags == wm::WM_MOUSEMOVE {
                let ddx = if x < 0 { x + MOVE_MOUSE_RELATIVE } else { x - MOVE_MOUSE_RELATIVE };
                let ddy = if y < 0 { y + MOVE_MOUSE_RELATIVE } else { y - MOVE_MOUSE_RELATIVE };
                (
                    (cx + ddx).clamp(desktop.left, desktop.right - 1),
                    (cy + ddy).clamp(desktop.top, desktop.bottom - 1),
                )
            } else {
                (cx, cy)
            }
        } else {
            from_universal(x, y, desktop)
        };
        let ev = match flags {
            wm::WM_MOUSEMOVE => Some(MouseEvent::Move { x: pos.0, y: pos.1, dx: 0, dy: 0 }),
            wm::WM_LBUTTONDOWN => Some(MouseEvent::Button { button: MouseButton::Left, down: true, x: pos.0, y: pos.1 }),
            wm::WM_LBUTTONUP => Some(MouseEvent::Button { button: MouseButton::Left, down: false, x: pos.0, y: pos.1 }),
            wm::WM_RBUTTONDOWN => Some(MouseEvent::Button { button: MouseButton::Right, down: true, x: pos.0, y: pos.1 }),
            wm::WM_RBUTTONUP => Some(MouseEvent::Button { button: MouseButton::Right, down: false, x: pos.0, y: pos.1 }),
            wm::WM_MBUTTONDOWN => Some(MouseEvent::Button { button: MouseButton::Middle, down: true, x: pos.0, y: pos.1 }),
            wm::WM_MBUTTONUP => Some(MouseEvent::Button { button: MouseButton::Middle, down: false, x: pos.0, y: pos.1 }),
            wm::WM_XBUTTONDOWN | wm::WM_XBUTTONUP => Some(MouseEvent::Button {
                button: if wheel == wm::XBUTTON2 { MouseButton::X2 } else { MouseButton::X1 },
                down: flags == wm::WM_XBUTTONDOWN,
                x: pos.0,
                y: pos.1,
            }),
            wm::WM_MOUSEWHEEL => Some(MouseEvent::Wheel { delta: wheel }),
            wm::WM_MOUSEHWHEEL => Some(MouseEvent::HWheel { delta: wheel }),
            _ => None,
        };
        // In relative mode the controlled machine detects its own edges (MoveToMyNeighbourIfNeeded
        // + SendNextMachine back to the controller).
        let mut next: Option<(u32, (i32, i32))> = None;
        if relative && flags == wm::WM_MOUSEMOVE && st.last_jump.elapsed() >= JUMP_THROTTLE {
            if let Some(dir) = edge_dir(pos.0, pos.1, desktop) {
                if let Some(new_id) = self.neighbour(&st, &self.my_name, dir) {
                    if new_id != self.my_id && st.last_next_machine.elapsed() >= JUMP_THROTTLE {
                        st.last_next_machine = Instant::now();
                        next = Some((new_id, jump_point(dir, pos.0, pos.1, desktop)));
                    }
                }
            }
        }
        drop(st);
        if let Some(ev) = ev {
            if let Err(e) = self.backend.inject_mouse(ev) {
                warn!("inject_mouse: {e:#}");
            }
        }
        if let Some((new_id, jp)) = next {
            let mut d = Data::new(ptype::NextMachine);
            d.set_md_x(jp.0);
            d.set_md_y(jp.1);
            d.set_md_wheel(new_id as i32);
            self.enqueue(src, d);
        }
    }

    fn on_clipboard_data(&self, conn: &Arc<Conn>, image: bool, data: &[u8]) {
        if image || !self.cfg.share_clipboard {
            return; // images not supported
        }
        match clipboard::decode_text(data) {
            Ok(t) => {
                if let Some(text) = t.text {
                    let mut st = self.st();
                    // The same data arrives once per socket (MWB keeps two per peer).
                    if let Some((last, at)) = &st.last_clip_in {
                        if *last == text && at.elapsed() < Duration::from_secs(2) {
                            return;
                        }
                    }
                    st.last_clip_in = Some((text.clone(), Instant::now()));
                    drop(st);
                    self.emit(EngineEvent::ClipboardText { from: conn.name(), text });
                }
            }
            Err(e) => warn!("bad clipboard data: {e:#}"),
        }
    }
}
