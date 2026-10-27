#![allow(dead_code)]
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const KEY: &str = "abcdEFGH1234!@#$";

/// Find dotnet; None => the interop tests are skipped (set MWB_REQUIRE_DOTNET=1 to fail instead).
pub fn dotnet() -> Option<String> {
    let cands = [std::env::var("DOTNET").ok(), Some("dotnet".into()), Some("/opt/homebrew/bin/dotnet".into()), Some("/usr/local/share/dotnet/dotnet".into())];
    for c in cands.into_iter().flatten() {
        if Command::new(&c).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
            return Some(c);
        }
    }
    if std::env::var("MWB_REQUIRE_DOTNET").is_ok() {
        panic!("dotnet not found");
    }
    eprintln!("dotnet not found: skipping C# interop test");
    None
}

/// Build the C# harness once; returns the path of the built dll.
pub fn harness() -> Option<(String, PathBuf)> {
    static BUILT: OnceLock<Option<(String, PathBuf)>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let dn = dotnet()?;
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("interop");
            let out = dir.join("bin").join("harness");
            let st = Command::new(&dn)
                .args(["build", "-c", "Release", "-nologo", "-v", "q", "-o"])
                .arg(&out)
                .current_dir(&dir)
                .status()
                .expect("run dotnet build");
            assert!(st.success(), "dotnet build of interop harness failed");
            Some((dn, out.join("MwbInterop.dll")))
        })
        .clone()
}

pub fn run_harness(args: &[&str]) -> Option<serde_json::Value> {
    let (dn, dll) = harness()?;
    let out = Command::new(dn).arg(dll).args(args).output().expect("run harness");
    assert!(out.status.success(), "harness failed: {}", String::from_utf8_lossy(&out.stderr));
    Some(serde_json::from_slice(&out.stdout).expect("harness json"))
}

pub struct Peer {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub lines: Receiver<serde_json::Value>,
    pub seen: Vec<serde_json::Value>,
}

impl Peer {
    pub fn spawn(args: &[&str]) -> Option<Peer> {
        let (dn, dll) = harness()?;
        let mut child =
            Command::new(dn).arg(dll).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().expect("spawn peer");
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                eprintln!("[c#] {line}");
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        Some(Peer { child, stdin, lines: rx, seen: Vec::new() })
    }

    /// Send a command line to the peer's stdin.
    pub fn send(&mut self, line: &str) {
        eprintln!("[c# <] {line}");
        let w = self.stdin.as_mut().expect("stdin");
        writeln!(w, "{line}").unwrap();
        w.flush().unwrap();
    }

    /// Collect lines for `d` (into `seen`) without waiting for anything in particular.
    pub fn drain_for(&mut self, d: Duration) {
        let end = Instant::now() + d;
        while let Ok(v) = self.lines.recv_timeout(end.saturating_duration_since(Instant::now())) {
            self.seen.push(v);
        }
    }

    /// Wait for a line matching `f` (lines seen meanwhile are kept in `seen`).
    pub fn wait_for(&mut self, timeout: Duration, f: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        if let Some(v) = self.seen.iter().find(|v| f(v)) {
            return v.clone();
        }
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(v) => {
                    self.seen.push(v.clone());
                    if f(&v) {
                        return v;
                    }
                }
                Err(_) => panic!("timeout waiting for C# peer output; seen: {:#?}", self.seen),
            }
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A base port P such that P+1 is (very likely) free.
pub fn free_base_port() -> u16 {
    loop {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        if p > 1 {
            return p - 1;
        }
    }
}

pub fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

pub fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Discovery candidates for tests: none (tests must never scan the network).
pub fn no_scan() -> std::sync::Arc<dyn mwb::discovery::CandidateProvider> {
    std::sync::Arc::new(mwb::discovery::StaticCandidates(Vec::new()))
}
