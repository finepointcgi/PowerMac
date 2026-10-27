//! Per-app firewall rules (Little Snitch / Radio Silence style): a model, an
//! evaluator, storage in `net-rules.json` and a Little Snitch `.lsrules`
//! importer.
//!
//! This is a model only. Nothing here blocks traffic: enforcing it needs a
//! Network Extension content filter on macOS (an entitlement only Apple's paid
//! developer program grants) or a WFP callout on Windows. What it does today
//! is answer "what would these rules do to this connection?" for the live
//! monitor (`powermac net audit`), so a rule set can be built and checked
//! against real traffic before anything enforces it.
//!
//! Evaluation follows Little Snitch: among the enabled rules that match, the
//! most specific one wins (a high-priority rule beats any regular one; then a
//! named app beats "any app", an exact host or address beats a domain, a
//! network or "local network", a port list beats any port, a protocol beats
//! any); between equally specific rules Deny beats Ask beats Allow. When
//! nothing matches, the set's default action applies (Allow, as in Radio
//! Silence, where you list what to block).

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{canonical_ip, Connection, Direction, ProcessInfo, Protocol, Scope, Snapshot};

/// File name of the rule set inside PowerMac's data folder.
pub const RULES_FILE: &str = "net-rules.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    #[default]
    Allow,
    /// Little Snitch's "ask": a future enforcer would prompt.
    Ask,
    Deny,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Allow => "allow",
            Action::Ask => "ask",
            Action::Deny => "deny",
        }
    }
}

/// Which app(s) a rule applies to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", tag = "type", content = "value")]
pub enum AppMatch {
    #[default]
    Any,
    /// Everything inside this `.app` bundle (helpers included).
    Bundle(PathBuf),
    /// Exactly this executable.
    Path(PathBuf),
    /// Any process with this executable or app name (case-insensitive), for
    /// tools that aren't in a bundle or move around (`curl`, `node`).
    Name(String),
}

impl AppMatch {
    /// A bundle if `path` is (or is inside) a `.app`, else the executable.
    pub fn for_path(path: &Path) -> AppMatch {
        let s = path.to_string_lossy();
        if s.to_ascii_lowercase().trim_end_matches('/').ends_with(".app") {
            return AppMatch::Bundle(PathBuf::from(s.trim_end_matches('/')));
        }
        match crate::model::outermost_bundle(path) {
            Some(b) => AppMatch::Bundle(b),
            None => AppMatch::Path(path.to_path_buf()),
        }
    }

    pub fn matches(&self, p: &ProcessInfo) -> bool {
        match self {
            AppMatch::Any => true,
            AppMatch::Bundle(b) => p.path.as_deref().is_some_and(|path| path.starts_with(b)),
            AppMatch::Path(x) => p.path.as_deref() == Some(x.as_path()),
            // The executable's name or its app's (so "Google Chrome" covers its helpers).
            AppMatch::Name(n) => {
                p.name.eq_ignore_ascii_case(n)
                    || p.path.as_deref().and_then(|x| x.file_name()).is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(n))
                    || p.bundle().and_then(|b| b.file_stem().map(|s| s.to_string_lossy().into_owned())).is_some_and(|s| s.eq_ignore_ascii_case(n))
            }
        }
    }

    fn specificity(&self) -> u8 {
        match self {
            AppMatch::Any => 0,
            AppMatch::Name(_) => 1,
            AppMatch::Bundle(_) => 2,
            AppMatch::Path(_) => 3,
        }
    }

    pub fn label(&self) -> String {
        match self {
            AppMatch::Any => "any app".into(),
            AppMatch::Bundle(b) => b.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| b.display().to_string()),
            AppMatch::Path(p) => p.display().to_string(),
            AppMatch::Name(n) => n.clone(),
        }
    }
}

/// Which remote end(s) a rule applies to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", tag = "type", content = "value")]
pub enum RemoteMatch {
    #[default]
    Any,
    /// Private, link-local and multicast addresses (and loopback).
    LocalNetwork,
    /// One address.
    Address(IpAddr),
    /// A network, e.g. `10.0.0.0/8`.
    Network { addr: IpAddr, prefix: u8 },
    /// Exactly this host name. Only matches connections whose host name is
    /// known (the socket table has addresses only).
    Host(String),
    /// This domain and all its subdomains.
    Domain(String),
}

impl RemoteMatch {
    /// `any`, `local-net`, `1.2.3.4`, `10.0.0.0/8`, `*.example.com` /
    /// `.example.com` (domain) or `host.example.com`.
    pub fn parse(s: &str) -> Result<RemoteMatch> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        match lower.as_str() {
            "" | "any" | "*" => return Ok(RemoteMatch::Any),
            "local-net" | "local" | "localnet" | "lan" => return Ok(RemoteMatch::LocalNetwork),
            _ => {}
        }
        if let Some((a, p)) = s.split_once('/') {
            let addr: IpAddr = a.trim().parse().with_context(|| format!("bad network address {a:?}"))?;
            let prefix: u8 = p.trim().parse().with_context(|| format!("bad prefix length {p:?}"))?;
            let max = if addr.is_ipv4() { 32 } else { 128 };
            if prefix > max {
                bail!("prefix /{prefix} is longer than {max}");
            }
            return Ok(RemoteMatch::Network { addr, prefix });
        }
        if let Ok(ip) = s.trim_matches(|c| c == '[' || c == ']').parse::<IpAddr>() {
            return Ok(RemoteMatch::Address(canonical_ip(ip)));
        }
        if let Some(d) = lower.strip_prefix("*.").or_else(|| lower.strip_prefix('.')) {
            return Ok(RemoteMatch::Domain(valid_host(d)?));
        }
        Ok(RemoteMatch::Host(valid_host(&lower)?))
    }

    pub fn matches(&self, ip: Option<IpAddr>, host: Option<&str>) -> bool {
        let ip = ip.map(canonical_ip);
        match self {
            RemoteMatch::Any => true,
            RemoteMatch::LocalNetwork => ip.is_some_and(|ip| Scope::of(ip) != Scope::Internet),
            RemoteMatch::Address(a) => ip == Some(canonical_ip(*a)),
            RemoteMatch::Network { addr, prefix } => ip.is_some_and(|ip| in_network(ip, *addr, *prefix)),
            RemoteMatch::Host(h) => host.is_some_and(|x| x.trim_end_matches('.').eq_ignore_ascii_case(h)),
            RemoteMatch::Domain(d) => host.is_some_and(|x| {
                let x = x.trim_end_matches('.').to_ascii_lowercase();
                x == *d || x.ends_with(&format!(".{d}"))
            }),
        }
    }

    fn specificity(&self) -> u8 {
        match self {
            RemoteMatch::Any => 0,
            RemoteMatch::LocalNetwork => 1,
            RemoteMatch::Network { .. } | RemoteMatch::Domain(_) => 2,
            RemoteMatch::Address(_) | RemoteMatch::Host(_) => 3,
        }
    }

    pub fn label(&self) -> String {
        match self {
            RemoteMatch::Any => "any server".into(),
            RemoteMatch::LocalNetwork => "local network".into(),
            RemoteMatch::Address(a) => a.to_string(),
            RemoteMatch::Network { addr, prefix } => format!("{addr}/{prefix}"),
            RemoteMatch::Host(h) => h.clone(),
            RemoteMatch::Domain(d) => format!("*.{d}"),
        }
    }
}

fn valid_host(s: &str) -> Result<String> {
    let s = s.trim().trim_end_matches('.').to_ascii_lowercase();
    if s.is_empty() || s.len() > 253 || !s.split('.').all(|l| !l.is_empty() && l.len() <= 63 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        bail!("{s:?} is not a host name, address or network");
    }
    Ok(s)
}

/// Whether `ip` is inside `addr/prefix` (IPv4 networks only match IPv4).
pub fn in_network(ip: IpAddr, addr: IpAddr, prefix: u8) -> bool {
    match (canonical_ip(ip), canonical_ip(addr)) {
        (IpAddr::V4(a), IpAddr::V4(n)) => {
            let p = prefix.min(32) as u32;
            let mask = if p == 0 { 0 } else { u32::MAX << (32 - p) };
            (u32::from(a) & mask) == (u32::from(n) & mask)
        }
        (IpAddr::V6(a), IpAddr::V6(n)) => {
            let p = prefix.min(128) as u32;
            let mask = if p == 0 { 0 } else { u128::MAX << (128 - p) };
            (u128::from(a) & mask) == (u128::from(n) & mask)
        }
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

/// Remote ports a rule applies to; empty means any.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Ports(pub Vec<PortRange>);

impl Ports {
    pub fn any() -> Self {
        Ports(Vec::new())
    }

    /// `any`, `443`, `80, 443`, `8000-8100`.
    pub fn parse(s: &str) -> Result<Ports> {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("any") || s == "*" {
            return Ok(Ports::any());
        }
        let mut v = Vec::new();
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (a, b) = part.split_once('-').unwrap_or((part, part));
            let start: u16 = a.trim().parse().with_context(|| format!("bad port {a:?}"))?;
            let end: u16 = b.trim().parse().with_context(|| format!("bad port {b:?}"))?;
            if start > end {
                bail!("port range {part} is backwards");
            }
            v.push(PortRange { start, end });
        }
        Ok(Ports(v))
    }

    pub fn is_any(&self) -> bool {
        self.0.is_empty()
    }

    pub fn matches(&self, port: Option<u16>) -> bool {
        self.is_any() || port.is_some_and(|p| self.0.iter().any(|r| r.start <= p && p <= r.end))
    }

    pub fn label(&self) -> String {
        if self.is_any() {
            return "any port".into();
        }
        self.0.iter().map(|r| if r.start == r.end { r.start.to_string() } else { format!("{}-{}", r.start, r.end) }).collect::<Vec<_>>().join(", ")
    }
}

/// Which direction(s) a rule applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum DirectionMatch {
    #[default]
    Outgoing,
    Incoming,
    Both,
}

impl DirectionMatch {
    pub fn matches(self, d: Direction) -> bool {
        matches!((self, d), (DirectionMatch::Both, _) | (DirectionMatch::Outgoing, Direction::Outgoing) | (DirectionMatch::Incoming, Direction::Incoming))
    }
}

/// One rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rule {
    pub id: String,
    pub enabled: bool,
    pub action: Action,
    pub app: AppMatch,
    pub direction: DirectionMatch,
    /// `None`: TCP and UDP.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<Protocol>,
    pub remote: RemoteMatch,
    #[serde(skip_serializing_if = "Ports::is_any")]
    pub ports: Ports,
    /// Wins over every regular rule (Little Snitch's "high priority").
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub high_priority: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Where an imported rule came from (the `.lsrules` file's name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Unix seconds.
    pub created: u64,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            id: String::new(),
            enabled: true,
            action: Action::Deny,
            app: AppMatch::Any,
            direction: DirectionMatch::Outgoing,
            protocol: None,
            remote: RemoteMatch::Any,
            ports: Ports::any(),
            high_priority: false,
            note: String::new(),
            source: None,
            created: 0,
        }
    }
}

impl Rule {
    /// Block (Radio Silence style) or allow everything an app sends.
    pub fn for_app(app: AppMatch, action: Action) -> Rule {
        Rule { app, action, ..Rule::default() }
    }

    pub fn matches(&self, q: &Query) -> bool {
        self.enabled
            && self.app.matches(q.process)
            && self.direction.matches(q.direction)
            && self.protocol.is_none_or(|p| p == q.protocol)
            && self.remote.matches(q.remote_ip, q.remote_host)
            && self.ports.matches(q.remote_port)
    }

    /// Bigger is more specific; compared lexicographically.
    pub fn specificity(&self) -> (bool, u8, u8, bool, bool, bool) {
        (
            self.high_priority,
            self.app.specificity(),
            self.remote.specificity(),
            !self.ports.is_any(),
            self.protocol.is_some(),
            self.direction != DirectionMatch::Both,
        )
    }

    /// "deny Safari → *.example.com, 443 (TCP)".
    pub fn summary(&self) -> String {
        let dir = match self.direction {
            DirectionMatch::Outgoing => "→",
            DirectionMatch::Incoming => "←",
            DirectionMatch::Both => "↔",
        };
        let mut s = format!("{} {} {dir} {}", self.action.label(), self.app.label(), self.remote.label());
        if !self.ports.is_any() {
            s.push_str(&format!(", port {}", self.ports.label()));
        }
        if let Some(p) = self.protocol {
            s.push_str(&format!(" ({})", p.label()));
        }
        if !self.enabled {
            s.push_str(" [off]");
        }
        s
    }
}

/// What a rule set is asked about.
#[derive(Clone, Copy, Debug)]
pub struct Query<'a> {
    pub process: &'a ProcessInfo,
    pub direction: Direction,
    pub protocol: Protocol,
    pub remote_ip: Option<IpAddr>,
    /// The remote host name when known (DNS, SNI); domain and host rules
    /// only match with one.
    pub remote_host: Option<&'a str>,
    pub remote_port: Option<u16>,
}

impl<'a> Query<'a> {
    pub fn for_connection(process: &'a ProcessInfo, c: &Connection) -> Self {
        Query {
            process,
            direction: c.direction,
            protocol: c.socket.protocol,
            remote_ip: c.socket.remote.map(|r| r.ip()),
            remote_host: None,
            remote_port: c.socket.remote.map(|r| r.port()),
        }
    }
}

/// The outcome for one query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub action: Action,
    /// The deciding rule; `None` means the default action applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
}

/// The whole rule set as stored in `net-rules.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RuleSet {
    pub version: u32,
    /// What happens to connections no rule matches.
    pub default_action: Action,
    pub rules: Vec<Rule>,
}

impl Default for RuleSet {
    fn default() -> Self {
        RuleSet { version: 1, default_action: Action::Allow, rules: Vec::new() }
    }
}

impl RuleSet {
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(RULES_FILE)
    }

    /// Load `<dir>/net-rules.json`; a missing file is an empty set.
    pub fn load(dir: &Path) -> Result<RuleSet> {
        let p = Self::path_in(dir);
        match std::fs::read(&p) {
            Ok(b) => serde_json::from_slice(&b).with_context(|| format!("reading {}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RuleSet::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
        }
    }

    /// Write `<dir>/net-rules.json` atomically.
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let p = Self::path_in(dir);
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &p).with_context(|| format!("replacing {}", p.display()))?;
        Ok(())
    }

    /// Add a rule, giving it an id and creation time if it has none; returns the id.
    pub fn add(&mut self, mut rule: Rule) -> String {
        if rule.id.is_empty() || self.rules.iter().any(|r| r.id == rule.id) {
            rule.id = self.next_id();
        }
        if rule.created == 0 {
            rule.created = now_unix();
        }
        let id = rule.id.clone();
        self.rules.push(rule);
        id
    }

    pub fn remove(&mut self, id: &str) -> Option<Rule> {
        let i = self.rules.iter().position(|r| r.id == id)?;
        Some(self.rules.remove(i))
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Rule> {
        self.rules.iter_mut().find(|r| r.id == id)
    }

    fn next_id(&self) -> String {
        let n = self.rules.iter().filter_map(|r| r.id.strip_prefix('r').and_then(|n| n.parse::<u64>().ok())).max().unwrap_or(0);
        format!("r{}", n + 1)
    }

    /// The most specific matching rule, or the default action.
    pub fn evaluate(&self, q: &Query) -> Verdict {
        let best = self.rules.iter().filter(|r| r.matches(q)).max_by(|a, b| a.specificity().cmp(&b.specificity()).then(a.action.cmp(&b.action)));
        match best {
            Some(r) => Verdict { action: r.action, rule: Some(r.id.clone()) },
            None => Verdict { action: self.default_action, rule: None },
        }
    }

    /// What the rules would do to every connection with a remote end in a
    /// snapshot (listeners are left out: rules judge traffic, not sockets).
    pub fn audit(&self, snap: &Snapshot) -> Vec<AuditEntry> {
        let mut out = Vec::new();
        for app in snap.by_app() {
            for c in &app.connections {
                if c.socket.remote.is_none() {
                    continue;
                }
                let fallback;
                let p = match snap.process(c.socket.pid) {
                    Some(p) => p,
                    None => {
                        fallback = ProcessInfo { pid: c.socket.pid, name: c.process.clone(), path: None };
                        &fallback
                    }
                };
                let verdict = self.evaluate(&Query::for_connection(p, c));
                out.push(AuditEntry { app: app.name.clone(), connection: c.clone(), verdict });
            }
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub app: String,
    pub connection: Connection,
    pub verdict: Verdict,
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Little Snitch .lsrules import
// ---------------------------------------------------------------------------

/// What an import produced.
#[derive(Debug, Default)]
pub struct Import {
    pub name: String,
    pub rules: Vec<Rule>,
    /// Entries that were skipped and why.
    pub skipped: Vec<String>,
}

/// Parse a Little Snitch rule group (`.lsrules`, JSON). Each remote host,
/// domain or address list entry becomes one rule; `denied-remote-*` lists
/// become deny rules for any app. Remotes this model can't express
/// (`bonjour`, `dns-servers`, `via` processes, …) are reported in
/// `skipped`.
pub fn import_lsrules(json: &str) -> Result<Import> {
    let v: serde_json::Value = serde_json::from_str(json).context("not a JSON .lsrules file")?;
    let obj = v.as_object().context(".lsrules must be a JSON object")?;
    let name = obj.get("name").and_then(|n| n.as_str()).unwrap_or("Little Snitch rules").to_string();
    let mut imp = Import { name: name.clone(), ..Import::default() };
    let src = Some(name.clone());

    if let Some(rules) = obj.get("rules") {
        let rules = rules.as_array().context("\"rules\" must be an array")?;
        for (i, r) in rules.iter().enumerate() {
            match import_one(r, &src) {
                Ok((mut v, skipped)) => {
                    imp.rules.append(&mut v);
                    imp.skipped.extend(skipped.into_iter().map(|s| format!("rule {}: {s}", i + 1)));
                }
                Err(e) => imp.skipped.push(format!("rule {}: {e:#}", i + 1)),
            }
        }
    }

    for (key, kind) in [("denied-remote-hosts", "host"), ("denied-remote-domains", "domain"), ("denied-remote-addresses", "address")] {
        for entry in string_list(obj.get(key)) {
            let remote = match kind {
                "domain" => valid_host(&entry).map(RemoteMatch::Domain),
                _ => RemoteMatch::parse(&entry),
            };
            match remote {
                Ok(remote) => imp.rules.push(Rule { remote, direction: DirectionMatch::Outgoing, action: Action::Deny, source: src.clone(), ..Rule::default() }),
                Err(e) => imp.skipped.push(format!("{key} {entry:?}: {e:#}")),
            }
        }
    }
    Ok(imp)
}

/// A string or array of strings, with comma-separated items split.
fn string_list(v: Option<&serde_json::Value>) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: &str| out.extend(s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from));
    match v {
        Some(serde_json::Value::String(s)) => push(s),
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).for_each(&mut push),
        _ => {}
    }
    out
}

fn import_one(r: &serde_json::Value, src: &Option<String>) -> Result<(Vec<Rule>, Vec<String>)> {
    let o = r.as_object().context("not an object")?;
    let s = |k: &str| o.get(k).and_then(|v| v.as_str()).map(str::trim);
    let mut skipped = Vec::new();

    let app = match s("process") {
        None | Some("any") | Some("") => AppMatch::Any,
        Some(p) => AppMatch::for_path(Path::new(p)),
    };
    if let Some(via) = s("via") {
        skipped.push(format!("\"via\" {via:?} isn't modelled; the rule applies to the process directly"));
    }
    let action = match s("action").unwrap_or("allow") {
        "allow" => Action::Allow,
        "deny" => Action::Deny,
        "ask" => Action::Ask,
        a => bail!("unknown action {a:?}"),
    };
    let direction = match s("direction").unwrap_or("outgoing") {
        "outgoing" => DirectionMatch::Outgoing,
        "incoming" => DirectionMatch::Incoming,
        d => bail!("unknown direction {d:?}"),
    };
    let protocol = match o.get("protocol") {
        None => None,
        Some(serde_json::Value::String(p)) => match p.to_ascii_lowercase().as_str() {
            "tcp" => Some(Protocol::Tcp),
            "udp" => Some(Protocol::Udp),
            "any" | "" => None,
            p => bail!("protocol {p:?} isn't TCP or UDP"),
        },
        Some(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(6) => Some(Protocol::Tcp),
            Some(17) => Some(Protocol::Udp),
            _ => bail!("protocol {n} isn't TCP or UDP"),
        },
        Some(p) => bail!("bad protocol {p}"),
    };
    let ports = match o.get("ports") {
        None => Ports::any(),
        Some(serde_json::Value::String(p)) => Ports::parse(p)?,
        Some(serde_json::Value::Number(n)) => Ports::parse(&n.to_string())?,
        Some(p) => bail!("bad ports {p}"),
    };
    let high_priority = s("priority") == Some("high");
    let enabled = !o.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false);
    let note = s("notes").unwrap_or("").to_string();
    let base = Rule { enabled, action, app, direction, protocol, ports, high_priority, note, source: src.clone(), ..Rule::default() };

    let mut remotes = Vec::new();
    for h in string_list(o.get("remote-hosts")) {
        match RemoteMatch::parse(&h) {
            Ok(m) => remotes.push(m),
            Err(e) => skipped.push(format!("remote host {h:?}: {e:#}")),
        }
    }
    for d in string_list(o.get("remote-domains")) {
        match valid_host(d.trim_start_matches("*.").trim_start_matches('.')) {
            Ok(d) => remotes.push(RemoteMatch::Domain(d)),
            Err(e) => skipped.push(format!("remote domain {d:?}: {e:#}")),
        }
    }
    for a in string_list(o.get("remote-addresses")) {
        // Little Snitch also allows "a.b.c.d-e.f.g.h" ranges.
        if a.contains('-') && !a.contains('/') {
            skipped.push(format!("address range {a:?} isn't supported; use a network (a.b.c.d/n)"));
            continue;
        }
        match RemoteMatch::parse(&a) {
            Ok(m @ (RemoteMatch::Address(_) | RemoteMatch::Network { .. })) => remotes.push(m),
            Ok(_) => skipped.push(format!("remote address {a:?} isn't an address or network")),
            Err(e) => skipped.push(format!("remote address {a:?}: {e:#}")),
        }
    }
    match s("remote") {
        None => {}
        Some("any") => remotes.push(RemoteMatch::Any),
        Some("local-net") => remotes.push(RemoteMatch::LocalNetwork),
        Some(other) => skipped.push(format!("remote {other:?} isn't modelled")),
    }
    let had_remote_keys = ["remote-hosts", "remote-domains", "remote-addresses", "remote"].iter().any(|k| o.contains_key(*k));
    if remotes.is_empty() {
        if had_remote_keys {
            // Every remote was unusable: importing as "any" would widen the rule.
            skipped.push("no usable remote; rule not imported".into());
            return Ok((Vec::new(), skipped));
        }
        remotes.push(RemoteMatch::Any);
    }
    Ok((remotes.into_iter().map(|remote| Rule { remote, ..base.clone() }).collect(), skipped))
}
