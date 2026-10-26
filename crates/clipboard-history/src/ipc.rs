//! How the picker process talks to the history service running in the tray
//! app: one JSON request per loopback TCP connection, authenticated with a
//! random token. The service writes `<data dir>/service.json` (`{port,
//! token, pid}`, mode 0600) while it runs and removes it when it stops.
//!
//! Having the service do every change keeps it the only writer of the
//! history (the picker never edits the files behind its back), and lets the
//! picker work with a memory-only history.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::store::ItemSummary;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Request {
    /// Items matching `query` (pinned first), at most `limit`.
    List {
        query: String,
        limit: usize,
    },
    /// An image item's thumbnail.
    Thumb {
        id: u64,
    },
    Pin {
        id: u64,
        pinned: bool,
    },
    Delete {
        id: u64,
    },
    Clear {
        keep_pinned: bool,
    },
    /// Put the item on the clipboard; then, if `paste`, synthesize Cmd/Ctrl+V
    /// once process `pid` is in front again (the picker activates it).
    Paste {
        id: u64,
        plain: bool,
        paste: bool,
        pid: i32,
    },
    Pause {
        paused: bool,
    },
    Status,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<ItemSummary>>,
    /// Base64 PNG.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub png: Option<String>,
    pub paused: bool,
    pub persist: bool,
    pub total: usize,
}

impl Response {
    pub fn ok() -> Self {
        Response {
            ok: true,
            ..Default::default()
        }
    }
    pub fn err(e: impl std::fmt::Display) -> Self {
        Response {
            ok: false,
            error: Some(e.to_string()),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    token: String,
    #[serde(flatten)]
    req: Request,
}

/// `service.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

pub fn endpoint_path(data_dir: &Path) -> PathBuf {
    data_dir.join("service.json")
}

const MAX_REQUEST: u64 = 64 * 1024;

fn random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut s = String::new();
    for i in 0..2u64 {
        // RandomState is seeded from the OS RNG.
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        s.push_str(&format!("{:016x}", h.finish()));
    }
    s
}

/// The listening side (in the tray app).
pub struct Server {
    listener: TcpListener,
    token: String,
    path: PathBuf,
}

impl Server {
    /// Listen on a random loopback port and publish `service.json`.
    pub fn bind(data_dir: &Path) -> Result<Server> {
        std::fs::create_dir_all(data_dir)?;
        let listener = TcpListener::bind("127.0.0.1:0").context("binding the picker port")?;
        let port = listener.local_addr()?.port();
        let token = random_token();
        let ep = Endpoint {
            port,
            token: token.clone(),
            pid: std::process::id(),
        };
        let path = endpoint_path(data_dir);
        crate::store::write_atomic(&path, serde_json::to_string(&ep)?.as_bytes())?;
        Ok(Server {
            listener,
            token,
            path,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("bound")
    }

    /// Serve until `stop` returns true (checked after every connection; wake
    /// a blocked accept with [`Server::wake`]).
    pub fn serve(&self, stop: impl Fn() -> bool, mut handle: impl FnMut(Request) -> Response) {
        for conn in self.listener.incoming() {
            if stop() {
                break;
            }
            let Ok(mut s) = conn else { continue };
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            let r = BufReader::new((&s).take(MAX_REQUEST)).read_line(&mut line);
            let resp = match r {
                Ok(_) => match serde_json::from_str::<Envelope>(&line) {
                    Ok(env) if env.token == self.token => handle(env.req),
                    Ok(_) => Response::err("bad token"),
                    Err(e) => Response::err(format!("bad request: {e}")),
                },
                Err(e) => Response::err(e),
            };
            if let Ok(mut j) = serde_json::to_string(&resp) {
                j.push('\n');
                let _ = s.write_all(j.as_bytes());
            }
            let _ = s.shutdown(Shutdown::Both);
        }
    }

    /// Unblock [`serve`](Self::serve) (connects to ourselves).
    pub fn wake(addr: SocketAddr) {
        let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(300));
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Only remove the file if it is still ours.
        if let Ok(ep) = read_endpoint_file(&self.path) {
            if ep.token == self.token {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn read_endpoint_file(p: &Path) -> Result<Endpoint> {
    Ok(serde_json::from_slice(&std::fs::read(p)?)?)
}

/// The picker's side.
#[derive(Debug, Clone)]
pub struct Client {
    ep: Endpoint,
}

impl Client {
    /// Connect to the running service, if there is one (checks it answers).
    pub fn connect(data_dir: &Path) -> Option<Client> {
        let ep = read_endpoint_file(&endpoint_path(data_dir)).ok()?;
        let c = Client { ep };
        c.call(&Request::Status).ok().filter(|r| r.ok).map(|_| c)
    }

    pub fn call(&self, req: &Request) -> Result<Response> {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.ep.port));
        let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
        s.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut j = serde_json::to_string(&Envelope {
            token: self.ep.token.clone(),
            req: req.clone(),
        })?;
        j.push('\n');
        s.write_all(j.as_bytes())?;
        let mut line = String::new();
        BufReader::new(s.take(256 << 20)).read_line(&mut line)?;
        let r: Response = serde_json::from_str(&line).map_err(|e| anyhow!("bad response: {e}"))?;
        Ok(r)
    }

    /// `call` that turns `ok: false` into an error.
    pub fn call_ok(&self, req: &Request) -> Result<Response> {
        let r = self.call(req)?;
        if !r.ok {
            bail!(r.error.unwrap_or_else(|| "request failed".into()));
        }
        Ok(r)
    }
}

// ---------------------------------------------------------------------------
// base64 (thumbnails over the wire)

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| B64.iter().position(|&x| x == c).map(|p| p as u32);
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for q in bytes.chunks(4) {
        let pad = q.iter().rev().take_while(|&&b| b == b'=').count();
        let mut n = 0u32;
        for (i, &b) in q.iter().enumerate() {
            let v = if b == b'=' && i >= 4 - pad {
                0
            } else {
                val(b)?
            };
            n = n << 6 | v;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn base64_round_trip() {
        for n in 0..40usize {
            let d: Vec<u8> = (0..n).map(|i| (i * 37 % 256) as u8).collect();
            assert_eq!(b64_decode(&b64_encode(&d)).unwrap(), d);
        }
        assert_eq!(b64_encode(b"Man"), "TWFu");
        assert_eq!(b64_encode(b"Ma"), "TWE=");
        assert!(b64_decode("TWF").is_none());
    }

    #[test]
    fn request_json() {
        let j = serde_json::to_string(&Request::Clear { keep_pinned: true }).unwrap();
        assert_eq!(j, r#"{"op":"clear","keepPinned":true}"#);
    }

    #[test]
    fn server_round_trip_and_token() {
        let dir = std::env::temp_dir().join(format!("pm-cliphist-ipc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let server = Server::bind(&dir).unwrap();
        let addr = server.addr();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let t = std::thread::spawn(move || {
            server.serve(
                || stop2.load(Ordering::SeqCst),
                |req| match req {
                    Request::Status => Response::ok(),
                    Request::Delete { id: 7 } => Response::ok(),
                    other => Response::err(format!("unexpected {other:?}")),
                },
            );
        });
        let c = Client::connect(&dir).expect("service reachable");
        assert!(c.call_ok(&Request::Delete { id: 7 }).is_ok());
        assert!(c.call_ok(&Request::Delete { id: 8 }).is_err());
        // Wrong token is refused.
        let bad = Client {
            ep: Endpoint {
                token: "nope".into(),
                ..c.ep.clone()
            },
        };
        assert_eq!(
            bad.call(&Request::Status).unwrap().error.as_deref(),
            Some("bad token")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(endpoint_path(&dir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o777, 0o600);
        }
        stop.store(true, Ordering::SeqCst);
        Server::wake(addr);
        t.join().unwrap();
        assert!(!endpoint_path(&dir).exists(), "removed on stop");
        assert!(Client::connect(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
