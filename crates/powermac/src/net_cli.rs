//! `powermac net …`: the network monitor and the firewall rules model
//! (crate `net-monitor`).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use net_monitor::rules::{self, DirectionMatch, Ports, Query};
use net_monitor::{Action, AppMatch, Event, Protocol, RemoteMatch, Rule, RuleSet};

#[derive(Subcommand)]
pub enum NetCmd {
    /// Every app's current TCP/UDP connections (read-only).
    List {
        /// Include listening and unconnected UDP sockets.
        #[arg(short, long)]
        all: bool,
        /// Print JSON (the per-app grouping).
        #[arg(long)]
        json: bool,
    },
    /// Print connections as they open and close (Ctrl+C to stop).
    Watch {
        /// Seconds between polls.
        #[arg(short, long, default_value_t = 1.0)]
        interval: f64,
        /// Include listening and unconnected UDP sockets.
        #[arg(short, long)]
        all: bool,
        /// One JSON event per line.
        #[arg(long)]
        json: bool,
    },
    /// What the rules would do to each current connection (nothing is blocked).
    Audit {
        /// Only show connections the rules would deny or ask about.
        #[arg(long)]
        blocked: bool,
        #[arg(long)]
        json: bool,
    },
    /// The per-app rules in net-rules.json.
    Rules {
        #[command(subcommand)]
        cmd: RulesCmd,
    },
}

#[derive(Subcommand)]
pub enum RulesCmd {
    /// List the rules.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add a rule. Without --remote/--port it covers all of the app's traffic
    /// (Radio Silence style).
    Add {
        /// allow, deny or ask.
        action: String,
        /// App bundle (/Applications/Safari.app), executable path, or an
        /// executable name (curl). Omit for any app.
        #[arg(long)]
        app: Option<String>,
        /// any, local-net, an address, a network (10.0.0.0/8), a host, or a
        /// domain with its subdomains (*.example.com).
        #[arg(long, default_value = "any")]
        remote: String,
        /// Remote ports: 443, "80,443", 8000-8100.
        #[arg(long, default_value = "any")]
        port: String,
        /// tcp or udp (default: both).
        #[arg(long)]
        protocol: Option<String>,
        /// outgoing, incoming or both.
        #[arg(long, default_value = "outgoing")]
        direction: String,
        /// Wins over every regular rule.
        #[arg(long)]
        high_priority: bool,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Remove a rule by id.
    Remove { id: String },
    /// Turn a rule on.
    Enable { id: String },
    /// Turn a rule off (kept in the file).
    Disable { id: String },
    /// What happens to connections no rule matches: allow, deny or ask.
    Default { action: String },
    /// Import a Little Snitch rule group (.lsrules).
    Import { file: PathBuf },
    /// Which rule decides a hypothetical connection.
    Check {
        /// Executable path or name.
        app: String,
        /// Remote address or host[:port], e.g. 17.253.144.10:443 or example.com:443.
        remote: String,
        #[arg(long, default_value = "tcp")]
        protocol: String,
        #[arg(long)]
        incoming: bool,
    },
}

pub fn run(config_path: &Path, cmd: NetCmd) -> Result<()> {
    let dir = pm_config::data_dir_for(config_path);
    match cmd {
        NetCmd::List { all, json } => list(all, json),
        NetCmd::Watch { interval, all, json } => watch(interval, all, json),
        NetCmd::Audit { blocked, json } => audit(&dir, blocked, json),
        NetCmd::Rules { cmd } => rules_cmd(&dir, cmd),
    }
}

fn snapshot() -> Result<net_monitor::Snapshot> {
    let snap = net_monitor::snapshot()?;
    if snap.inaccessible > 0 && cfg!(target_os = "macos") {
        eprintln!("({} processes of other users or the system weren't readable; run with sudo to include them)", snap.inaccessible);
    }
    Ok(snap)
}

fn list(all: bool, json: bool) -> Result<()> {
    let snap = snapshot()?;
    let mut apps = snap.by_app();
    if !all {
        for a in &mut apps {
            a.connections.retain(|c| c.socket.remote.is_some());
        }
        apps.retain(|a| !a.connections.is_empty());
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&apps)?);
        return Ok(());
    }
    if apps.is_empty() {
        println!("no connections{}", if all { "" } else { " (--all includes listening sockets)" });
    }
    for a in &apps {
        let pids = a.pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ");
        println!("{}  (pid {pids}; {} connections, {} listening)", a.name, a.active(), a.listeners());
        for c in &a.connections {
            let scope = c.socket.remote_scope().map(|s| format!("  [{}]", s.label())).unwrap_or_default();
            let dir = if c.direction == net_monitor::Direction::Incoming && c.socket.remote.is_some() { "  incoming" } else { "" };
            println!("    {}{dir}{scope}", net_monitor::describe(&c.socket));
        }
    }
    Ok(())
}

fn watch(interval: f64, all: bool, json: bool) -> Result<()> {
    if !(0.1..=3600.0).contains(&interval) {
        bail!("--interval must be between 0.1 and 3600 seconds");
    }
    let stop = Arc::new(AtomicBool::new(false));
    eprintln!("watching connections every {interval} s (Ctrl+C to stop)");
    net_monitor::watch(Duration::from_secs_f64(interval), all, stop, |_, events| {
        for e in events {
            if json {
                if let Ok(s) = serde_json::to_string(e) {
                    println!("{s}");
                }
                continue;
            }
            let t = chrono::Local::now().format("%H:%M:%S");
            match e {
                Event::Opened { socket, process, direction } => {
                    let d = if *direction == net_monitor::Direction::Incoming && socket.remote.is_some() { " (incoming)" } else { "" };
                    println!("{t} + {}[{}] {}{d}", process.display_name(), process.pid, net_monitor::describe(socket));
                }
                Event::Closed { socket, process, open_for_ms } => {
                    println!("{t} - {}[{}] {} after {:.1} s", process.display_name(), process.pid, net_monitor::describe(socket), *open_for_ms as f64 / 1000.0);
                }
                Event::StateChanged { socket, process, .. } => {
                    println!("{t} ~ {}[{}] {}", process.display_name(), process.pid, net_monitor::describe(socket));
                }
            }
        }
    });
    Ok(())
}

fn audit(dir: &Path, blocked: bool, json: bool) -> Result<()> {
    let set = RuleSet::load(dir)?;
    let snap = snapshot()?;
    let mut entries = set.audit(&snap);
    let total = entries.len();
    let would_block = entries.iter().filter(|e| e.verdict.action == Action::Deny).count();
    let would_ask = entries.iter().filter(|e| e.verdict.action == Action::Ask).count();
    if blocked {
        entries.retain(|e| e.verdict.action != Action::Allow);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    if set.rules.is_empty() {
        println!("no rules yet (default: {}); add one with `powermac net rules add deny --app <app>`", set.default_action.label());
    }
    for e in &entries {
        let why = e.verdict.rule.as_deref().map(|r| format!("rule {r}")).unwrap_or_else(|| "default".into());
        println!("{:<5} {:<24} {}  ({why})", e.verdict.action.label(), e.app, net_monitor::describe(&e.connection.socket));
    }
    println!("{total} connections: {would_block} would be denied, {would_ask} would ask (rules are not enforced; nothing was blocked)");
    Ok(())
}

fn parse_action(s: &str) -> Result<Action> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "allow" => Action::Allow,
        "deny" | "block" => Action::Deny,
        "ask" => Action::Ask,
        _ => bail!("action must be allow, deny or ask (not {s:?})"),
    })
}

fn parse_protocol(s: &str) -> Result<Protocol> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "tcp" => Protocol::Tcp,
        "udp" => Protocol::Udp,
        _ => bail!("protocol must be tcp or udp (not {s:?})"),
    })
}

fn parse_app(s: &str) -> AppMatch {
    if s.contains('/') || s.contains('\\') {
        AppMatch::for_path(Path::new(s))
    } else {
        AppMatch::Name(s.to_string())
    }
}

fn print_rule(r: &Rule) {
    let src = r.source.as_deref().map(|s| format!("  (from {s})")).unwrap_or_default();
    let note = if r.note.is_empty() { String::new() } else { format!("  — {}", r.note) };
    let hp = if r.high_priority { " !" } else { "" };
    println!("{:<5} {}{hp}{note}{src}", r.id, r.summary());
}

fn rules_cmd(dir: &Path, cmd: RulesCmd) -> Result<()> {
    let mut set = RuleSet::load(dir)?;
    let path = RuleSet::path_in(dir);
    match cmd {
        RulesCmd::List { json } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&set)?);
                return Ok(());
            }
            println!("{} rules in {} (default: {}; not enforced)", set.rules.len(), path.display(), set.default_action.label());
            set.rules.iter().for_each(print_rule);
            Ok(())
        }
        RulesCmd::Add { action, app, remote, port, protocol, direction, high_priority, note } => {
            let direction = match direction.to_ascii_lowercase().as_str() {
                "outgoing" | "out" => DirectionMatch::Outgoing,
                "incoming" | "in" => DirectionMatch::Incoming,
                "both" => DirectionMatch::Both,
                d => bail!("direction must be outgoing, incoming or both (not {d:?})"),
            };
            let rule = Rule {
                action: parse_action(&action)?,
                app: app.as_deref().map(parse_app).unwrap_or_default(),
                remote: RemoteMatch::parse(&remote)?,
                ports: Ports::parse(&port)?,
                protocol: protocol.as_deref().map(parse_protocol).transpose()?,
                direction,
                high_priority,
                note,
                ..Rule::default()
            };
            let id = set.add(rule);
            set.save(dir)?;
            print_rule(set.rules.iter().find(|r| r.id == id).unwrap());
            Ok(())
        }
        RulesCmd::Remove { id } => {
            let r = set.remove(&id).with_context(|| format!("no rule {id}"))?;
            set.save(dir)?;
            println!("removed {} ({})", r.id, r.summary());
            Ok(())
        }
        RulesCmd::Enable { id } | RulesCmd::Disable { id } if set.get_mut(&id).is_none() => bail!("no rule {id}"),
        RulesCmd::Enable { id } => {
            set.get_mut(&id).unwrap().enabled = true;
            set.save(dir)
        }
        RulesCmd::Disable { id } => {
            set.get_mut(&id).unwrap().enabled = false;
            set.save(dir)
        }
        RulesCmd::Default { action } => {
            set.default_action = parse_action(&action)?;
            set.save(dir)?;
            println!("connections no rule matches: {}", set.default_action.label());
            Ok(())
        }
        RulesCmd::Import { file } => {
            let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
            let imp = rules::import_lsrules(&text)?;
            let n = imp.rules.len();
            for r in imp.rules {
                set.add(r);
            }
            set.save(dir)?;
            println!("imported {n} rules from \"{}\"", imp.name);
            for s in &imp.skipped {
                println!("  skipped: {s}");
            }
            Ok(())
        }
        RulesCmd::Check { app, remote, protocol, incoming } => {
            let process = if app.contains('/') || app.contains('\\') {
                let p = PathBuf::from(&app);
                let name = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| app.clone());
                net_monitor::ProcessInfo { pid: 0, name, path: Some(p) }
            } else {
                net_monitor::ProcessInfo { pid: 0, name: app.clone(), path: None }
            };
            let (ip, host, port) = parse_remote(&remote)?;
            let q = Query {
                process: &process,
                direction: if incoming { net_monitor::Direction::Incoming } else { net_monitor::Direction::Outgoing },
                protocol: parse_protocol(&protocol)?,
                remote_ip: ip,
                remote_host: host.as_deref(),
                remote_port: port,
            };
            let v = set.evaluate(&q);
            match v.rule.as_deref().and_then(|id| set.rules.iter().find(|r| r.id == id)) {
                Some(r) => println!("{} (rule {}: {})", v.action.label(), r.id, r.summary()),
                None => println!("{} (default)", v.action.label()),
            }
            Ok(())
        }
    }
}

/// `1.2.3.4`, `1.2.3.4:443`, `[::1]:443`, `example.com`, `example.com:443`.
fn parse_remote(s: &str) -> Result<(Option<std::net::IpAddr>, Option<String>, Option<u16>)> {
    if let Ok(sa) = s.parse::<std::net::SocketAddr>() {
        return Ok((Some(sa.ip()), None, Some(sa.port())));
    }
    if let Ok(ip) = s.trim_matches(|c| c == '[' || c == ']').parse::<std::net::IpAddr>() {
        return Ok((Some(ip), None, None));
    }
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (h, Some(p.parse::<u16>().with_context(|| format!("bad port in {s:?}"))?)),
        None => (s, None),
    };
    match RemoteMatch::parse(host)? {
        RemoteMatch::Host(h) => Ok((None, Some(h), port)),
        _ => bail!("{s:?} isn't an address or host name"),
    }
}
