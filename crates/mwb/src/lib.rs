//! Mouse Without Borders (PowerToys) protocol + engine, wire-compatible with the
//! Windows implementation.
//!
//! * [`protocol`] — the `DATA` package layout, package types, magic/checksum stamping.
//! * [`crypto`] — key normalization/generation, 24-bit magic hash, PBKDF2 + AES-256-CBC stream.
//! * [`transport`] — encrypted package framing over a byte stream.
//! * [`clipboard`] — clipboard text serialization (deflate + UTF-16 + chunking).
//! * [`discovery`] — finding machines with the same key on the local network.
//! * [`llmnr`] — an LLMNR responder so Windows finds this machine by its MWB name.
//! * [`matrix`] — machine layout and neighbour lookup.
//! * [`engine`] — sockets, handshake, switching, forwarding and injection.
//! * [`mock`] — an in-memory [`pm_input::InputBackend`] for tests.

pub mod clipboard;
pub mod config;
pub mod crypto;
pub mod discovery;
pub mod engine;
pub mod llmnr;
pub mod matrix;
pub mod mock;
pub mod protocol;
pub mod transport;

pub use config::{Config, DEFAULT_PORT};
pub use crypto::{create_random_key, get_24bit_hash, normalize_key};
pub use engine::{peer_spec, DiscoveredPeer, Engine, EngineEvent, EngineHandle, EngineMonitor, IncomingAttempt, PeerStatus, Status};
pub use matrix::{Dir, Matrix};
