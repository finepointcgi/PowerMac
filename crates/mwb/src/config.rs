use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::matrix::Matrix;

/// Default MWB base port (`TCPPort`). The message socket listens on `port + 1`,
/// the (unimplemented) clipboard/file socket on `port`.
pub const DEFAULT_PORT: u16 = 15100;

/// Engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Shared security key (whitespace is stripped, must be >= 16 chars).
    pub key: String,
    /// This machine's name as announced on the wire (max 32 ASCII chars).
    /// Empty = the host name.
    pub machine_name: String,
    /// This machine's 32-bit id. `None` = random per start (MWB persists it in settings).
    pub machine_id: Option<u32>,
    /// Up to 4 machine names in layout order (should include this machine).
    pub matrix: Vec<String>,
    /// One row (true, MWB default) or 2x2 grid.
    pub one_row: bool,
    /// Wrap around at the ends of the matrix (`MatrixCircle`).
    pub wrap_around: bool,
    /// Hide the local cursor while controlling another machine.
    pub hide_cursor_when_away: bool,
    /// MWB "Move mouse relatively" mode.
    pub move_mouse_relatively: bool,
    /// Switch machines when the cursor hits a screen edge (MWB "Easy Mouse").
    pub easy_mouse: bool,
    /// Base TCP port.
    pub port: u16,
    /// Local listen address.
    pub bind_address: String,
    /// Peer name -> address (`host`, `ip`, `host:baseport`). Peers missing here are
    /// resolved by name, as MWB does.
    pub peers: HashMap<String, String>,
    /// Exchange clipboard text with peers.
    pub share_clipboard: bool,
    /// Adopt `Matrix` packages sent by peers (MWB always does).
    pub accept_remote_matrix: bool,
    /// Swap Cmd(Win) and Ctrl on forwarded and injected keystrokes. Enable on a Mac
    /// talking to Windows peers so Cmd+C arrives as Ctrl+C and Ctrl+C from Windows
    /// acts as Cmd+C here. Default: true on macOS, false elsewhere.
    pub swap_cmd_ctrl: bool,
    pub heartbeat_secs: u64,
    pub reconnect_secs: u64,
    /// Look for machines with the same key on the local network and connect to
    /// them (see [`crate::discovery`]).
    pub auto_discover: bool,
    /// Seconds between discovery sweeps while no peer is connected.
    pub discovery_interval_secs: u64,
    /// Seconds between discovery sweeps once at least one peer is connected.
    pub discovery_interval_connected_secs: u64,
    /// Answer LLMNR name queries (UDP 5355) for `machine_name`, so Windows /
    /// PowerToys finds this machine by name (see [`crate::llmnr`]).
    #[serde(alias = "answerNameQueries")]
    pub answer_name_queries: bool,
    /// UDP port of the LLMNR responder (5355; other values only make sense in tests).
    pub llmnr_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            key: String::new(),
            machine_name: String::new(),
            machine_id: None,
            matrix: Vec::new(),
            one_row: true,
            wrap_around: false,
            hide_cursor_when_away: true,
            move_mouse_relatively: false,
            easy_mouse: true,
            port: DEFAULT_PORT,
            bind_address: "0.0.0.0".into(),
            peers: HashMap::new(),
            share_clipboard: true,
            accept_remote_matrix: true,
            swap_cmd_ctrl: cfg!(target_os = "macos"),
            heartbeat_secs: 60,
            reconnect_secs: 5,
            auto_discover: true,
            discovery_interval_secs: 60,
            discovery_interval_connected_secs: 300,
            answer_name_queries: true,
            llmnr_port: crate::llmnr::PORT,
        }
    }
}

impl Config {
    pub fn matrix(&self) -> Matrix {
        Matrix::from_names(&self.matrix, self.one_row, self.wrap_around)
    }

    /// Whether going from `self` to `new` needs a new engine (the listening
    /// socket, the key or this machine's identity changed). Everything else can
    /// be applied to a running engine with [`crate::EngineHandle::update_config`].
    pub fn needs_restart(&self, new: &Config) -> bool {
        crate::crypto::normalize_key(&self.key) != crate::crypto::normalize_key(&new.key)
            || self.effective_machine_name() != new.effective_machine_name()
            || self.machine_id != new.machine_id
            || self.port != new.port
            || self.bind_address.trim() != new.bind_address.trim()
    }

    /// Effective machine name (host name fallback, truncated to 32 chars like `GetMachineName`).
    pub fn effective_machine_name(&self) -> String {
        let n = if self.machine_name.trim().is_empty() {
            gethostname::gethostname().to_string_lossy().to_string()
        } else {
            self.machine_name.clone()
        };
        // Strip a domain suffix such as ".local" (macOS host names).
        let n = n.split('.').next().unwrap_or("").to_string();
        let n: String = n.chars().take(32).collect();
        n.trim().to_string()
    }
}
