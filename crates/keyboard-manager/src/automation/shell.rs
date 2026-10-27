//! Shell command actions: run a command or a script the user configured,
//! with a chosen interpreter, working directory and environment, in the
//! background (default) or waiting for it, with a timeout that kills the
//! whole process tree, an optional notification when it finishes, and its
//! output appended to a per-action log file.
//!
//! JSON (an action `{"type": "shell", ...}`; every field optional except
//! one of `command` / `scriptPath`):
//!
//! ```json
//! {
//!   "name": "Rebuild docs",
//!   "command": "make docs",
//!   "scriptPath": null,
//!   "args": [],
//!   "interpreter": "zsh",
//!   "workingDir": "~/src/site",
//!   "env": { "MODE": "fast" },
//!   "wait": false,
//!   "timeoutMs": 60000,
//!   "notify": "onFailure",
//!   "logOutput": true
//! }
//! ```
//!
//! `interpreter` is one of `"zsh"` (default), `"bash"`, `"sh"`, `"python3"`,
//! `"osascript"` (macOS), `"cmd"`, `"powershell"` (Windows; `pwsh`
//! elsewhere) or `{"custom": {"program": "/opt/homebrew/bin/fish", "args":
//! ["-c"]}}` (the command text or script path is appended after `args`,
//! then the action's own `args`).
//!
//! Timeouts: `timeoutMs` absent = 30 s when waiting, none in the background;
//! `0` = never. On timeout the process group (Unix: the command runs in its
//! own process group) or job object (Windows) gets SIGTERM / is terminated,
//! then SIGKILL after one second.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::launch::expand_path;

/// Default timeout of a shell action that is waited for.
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a timed-out process tree gets between SIGTERM and SIGKILL.
pub const KILL_GRACE: Duration = Duration::from_secs(1);
/// Output kept in memory per stream (the log file gets everything).
pub const CAPTURE_LIMIT: usize = 256 * 1024;
/// A per-action log is rotated (to `.log.1`) beyond this size.
pub const LOG_ROTATE_BYTES: u64 = 1024 * 1024;

/// A configured shell command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShellAction {
    /// Label for notifications and the log file name (optional).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Inline command text (`zsh -c <command>`, `python3 -c`, `osascript -e`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// Run this script file instead of `command`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_path: Option<String>,
    /// Arguments for the script / command (`$1`.. for shells).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    pub interpreter: Interpreter,
    /// Defaults to the home directory. `~/` is expanded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// Added to (or overriding) the inherited environment.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Wait for the command to finish (`false`: run in the background).
    pub wait: bool,
    /// See the module docs; `0` = no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// When to show a notification with the exit status and the first
    /// output line. Also accepts `true` (= always) / `false`.
    pub notify: NotifyWhen,
    /// Append the output to `<log dir>/actions/<name>.log`.
    pub log_output: bool,
}

impl Default for ShellAction {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            script_path: None,
            args: Vec::new(),
            interpreter: Interpreter::default(),
            working_dir: None,
            env: BTreeMap::new(),
            wait: false,
            timeout_ms: None,
            notify: NotifyWhen::Never,
            log_output: true,
        }
    }
}

/// The program that runs the command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Interpreter {
    #[default]
    Zsh,
    Bash,
    Sh,
    Python3,
    /// AppleScript (macOS).
    Osascript,
    /// `cmd.exe /D /C` (Windows).
    Cmd,
    /// Windows PowerShell (`pwsh` elsewhere).
    Powershell,
    /// `program args... <command or script> <action args...>`.
    Custom {
        program: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
    },
}

impl Interpreter {
    pub fn label(&self) -> &'static str {
        match self {
            Interpreter::Zsh => "zsh",
            Interpreter::Bash => "bash",
            Interpreter::Sh => "sh",
            Interpreter::Python3 => "Python 3",
            Interpreter::Osascript => "AppleScript",
            Interpreter::Cmd => "cmd",
            Interpreter::Powershell => "PowerShell",
            Interpreter::Custom { .. } => "Custom",
        }
    }

    /// The interpreters offered on this platform (Custom with no program).
    pub fn all() -> Vec<Interpreter> {
        let mut v = vec![
            Interpreter::Zsh,
            Interpreter::Bash,
            Interpreter::Sh,
            Interpreter::Python3,
        ];
        if cfg!(target_os = "macos") {
            v.push(Interpreter::Osascript);
        }
        if cfg!(windows) {
            v.push(Interpreter::Cmd);
        }
        v.push(Interpreter::Powershell);
        v.push(Interpreter::Custom {
            program: String::new(),
            args: Vec::new(),
        });
        v
    }
}

/// When a finished shell action shows a notification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NotifyWhen {
    #[default]
    Never,
    Always,
    OnFailure,
}

impl<'de> Deserialize<'de> for NotifyWhen {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Bool(bool),
            Str(String),
        }
        Ok(match Repr::deserialize(d)? {
            Repr::Bool(true) => NotifyWhen::Always,
            Repr::Bool(false) => NotifyWhen::Never,
            Repr::Str(s) => match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
                "never" | "off" | "none" => NotifyWhen::Never,
                "always" | "on" => NotifyWhen::Always,
                "onfailure" | "failure" | "onerror" => NotifyWhen::OnFailure,
                _ => {
                    return Err(serde::de::Error::custom(format!(
                        "unknown notify value '{s}'"
                    )))
                }
            },
        })
    }
}

/// How a shell run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellResult {
    /// Exit code (`None` when killed by a signal).
    pub exit_code: Option<i32>,
    /// Unix signal that ended the process.
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    /// Output beyond [`CAPTURE_LIMIT`] was dropped from `stdout`/`stderr`.
    pub truncated: bool,
    pub duration: Duration,
    pub log: Option<PathBuf>,
}

impl ShellResult {
    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }

    /// First non-empty line of stdout, else of stderr.
    pub fn first_line(&self) -> Option<&str> {
        [&self.stdout, &self.stderr]
            .into_iter()
            .find_map(|s| s.lines().map(str::trim).find(|l| !l.is_empty()))
    }

    /// `"exit 0"`, `"exit 2"`, `"killed by signal 9"`, `"timed out after 5 s"`.
    pub fn status_text(&self) -> String {
        if self.timed_out {
            format!("timed out after {}", fmt_duration(self.duration))
        } else if let Some(c) = self.exit_code {
            format!("exit {c}")
        } else if let Some(s) = self.signal {
            format!("killed by signal {s}")
        } else {
            "ended".into()
        }
    }
}

/// Shows a notification: `(title, body)`.
pub type Notifier = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Where logs go and how notifications are shown.
#[derive(Clone, Default)]
pub struct RunContext {
    /// The app's log folder; per-action logs go to `<dir>/actions/`.
    /// `None`: no log files.
    pub log_dir: Option<PathBuf>,
    /// `(title, body)`; `None`: the platform default ([`default_notify`]).
    pub notifier: Option<Notifier>,
}

impl std::fmt::Debug for RunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunContext")
            .field("log_dir", &self.log_dir)
            .field("notifier", &self.notifier.is_some())
            .finish()
    }
}

impl RunContext {
    fn notify(&self, title: &str, body: &str) {
        match &self.notifier {
            Some(n) => n(title, body),
            None => default_notify(title, body),
        }
    }
}

/// macOS: a user notification through `osascript` (`display notification`);
/// elsewhere it is only logged.
pub fn default_notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            esc(body),
            esc(title)
        );
        match Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut c) => {
                let _ = std::thread::Builder::new()
                    .name("km-notify".into())
                    .spawn(move || {
                        let _ = c.wait();
                    });
            }
            Err(e) => log::warn!("automation: notification failed: {e}"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    log::info!("automation: {title}: {body}");
}

enum Source<'a> {
    Inline(&'a str),
    Script(String),
}

impl ShellAction {
    /// `name`, or a short form of the command.
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        if let Some(p) = self.script_path.as_deref().filter(|p| !p.trim().is_empty()) {
            return Path::new(p.trim())
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string());
        }
        let line = self
            .command
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        let mut s: String = line.chars().take(40).collect();
        if line.chars().count() > 40 {
            s.push('…');
        }
        if s.is_empty() {
            "Shell command".into()
        } else {
            s
        }
    }

    /// The timeout that applies (see the module docs).
    pub fn effective_timeout(&self) -> Option<Duration> {
        match self.timeout_ms {
            Some(0) => None,
            Some(ms) => Some(Duration::from_millis(ms)),
            None if self.wait => Some(DEFAULT_WAIT_TIMEOUT),
            None => None,
        }
    }

    /// Configuration problems that make the action fail to start.
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Err(e) = self.source() {
            out.push(e);
        }
        if let Interpreter::Custom { program, .. } = &self.interpreter {
            if program.trim().is_empty() {
                out.push("custom interpreter needs a program".into());
            }
        }
        if cfg!(not(target_os = "macos")) && self.interpreter == Interpreter::Osascript {
            out.push("osascript is only available on macOS".into());
        }
        if cfg!(not(windows)) && self.interpreter == Interpreter::Cmd {
            out.push("cmd is only available on Windows".into());
        }
        if let Some(p) = self.script_path.as_deref().filter(|p| !p.trim().is_empty()) {
            if !Path::new(&expand_path(p.trim())).is_file() {
                out.push(format!("script not found: {p}"));
            }
        }
        if let Some(wd) = self.working_dir.as_deref().filter(|w| !w.trim().is_empty()) {
            if !Path::new(&expand_path(wd.trim())).is_dir() {
                out.push(format!("working directory not found: {wd}"));
            }
        }
        for k in self.env.keys() {
            if k.is_empty() || k.contains('=') || k.contains('\0') {
                out.push(format!("invalid environment variable name '{k}'"));
            }
        }
        out
    }

    fn source(&self) -> Result<Source<'_>, String> {
        let script = self
            .script_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let inline = (!self.command.trim().is_empty()).then_some(self.command.as_str());
        match (inline, script) {
            (Some(_), Some(_)) => Err("set either command or scriptPath, not both".into()),
            (None, None) => Err("no command or scriptPath".into()),
            (Some(c), None) => Ok(Source::Inline(c)),
            (None, Some(p)) => Ok(Source::Script(expand_path(p))),
        }
    }

    /// The process to start (program, arguments, cwd, environment).
    pub fn command(&self) -> Result<Command, String> {
        let src = self.source()?;
        let dollar0 = "powermac-action".to_string();
        let (program, mut argv): (String, Vec<String>) = match &self.interpreter {
            Interpreter::Zsh | Interpreter::Bash | Interpreter::Sh => {
                let name = match self.interpreter {
                    Interpreter::Zsh => "zsh",
                    Interpreter::Bash => "bash",
                    _ => "sh",
                };
                let prog = if cfg!(unix) {
                    format!("/bin/{name}")
                } else {
                    name.to_string()
                };
                match &src {
                    Source::Inline(c) => (prog, vec!["-c".into(), c.to_string(), dollar0]),
                    Source::Script(p) => (prog, vec![p.clone()]),
                }
            }
            Interpreter::Python3 => {
                let (prog, mut pre) = if cfg!(windows) {
                    ("python".to_string(), vec![])
                } else {
                    ("/usr/bin/env".to_string(), vec!["python3".to_string()])
                };
                match &src {
                    Source::Inline(c) => pre.extend(["-c".to_string(), c.to_string()]),
                    Source::Script(p) => pre.push(p.clone()),
                }
                (prog, pre)
            }
            Interpreter::Osascript => {
                if cfg!(not(target_os = "macos")) {
                    return Err("osascript is only available on macOS".into());
                }
                match &src {
                    Source::Inline(c) => (
                        "/usr/bin/osascript".into(),
                        vec!["-e".into(), c.to_string()],
                    ),
                    Source::Script(p) => ("/usr/bin/osascript".into(), vec![p.clone()]),
                }
            }
            Interpreter::Cmd => {
                if cfg!(not(windows)) {
                    return Err("cmd is only available on Windows".into());
                }
                let v = match &src {
                    Source::Inline(c) => vec!["/D".into(), "/C".into(), c.to_string()],
                    Source::Script(p) => vec!["/D".into(), "/C".into(), p.clone()],
                };
                ("cmd.exe".into(), v)
            }
            Interpreter::Powershell => {
                let prog = if cfg!(windows) {
                    "powershell.exe"
                } else {
                    "pwsh"
                };
                let mut v: Vec<String> = vec!["-NoProfile".into(), "-NonInteractive".into()];
                match &src {
                    Source::Inline(c) => v.extend(["-Command".into(), c.to_string()]),
                    Source::Script(p) => v.extend([
                        "-ExecutionPolicy".into(),
                        "Bypass".into(),
                        "-File".into(),
                        p.clone(),
                    ]),
                }
                (prog.into(), v)
            }
            Interpreter::Custom { program, args } => {
                if program.trim().is_empty() {
                    return Err("custom interpreter needs a program".into());
                }
                let mut v = args.clone();
                match &src {
                    Source::Inline(c) => v.push(c.to_string()),
                    Source::Script(p) => v.push(p.clone()),
                }
                (expand_path(program.trim()), v)
            }
        };
        argv.extend(self.args.iter().cloned());
        let mut cmd = Command::new(program);
        cmd.args(argv);
        let wd = match self
            .working_dir
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
        {
            Some(w) => {
                let p = PathBuf::from(expand_path(w));
                if !p.is_dir() {
                    return Err(format!("working directory not found: {w}"));
                }
                Some(p)
            }
            None => home_dir().filter(|h| h.is_dir()),
        };
        if let Some(wd) = wd {
            cmd.current_dir(wd);
        }
        if !self.env.contains_key("PATH") {
            if let Some(p) = augmented_path(std::env::var_os("PATH")) {
                cmd.env("PATH", p);
            }
        }
        cmd.env("POWERMAC_ACTION", self.display_name());
        cmd.envs(&self.env);
        Ok(cmd)
    }

    /// The per-action log file for `log_dir`.
    pub fn log_path(&self, log_dir: &Path) -> PathBuf {
        log_dir
            .join("actions")
            .join(format!("{}.log", self.log_stem()))
    }

    fn log_stem(&self) -> String {
        let slug = slug(&self.name);
        if !slug.is_empty() {
            return slug;
        }
        let key = format!(
            "{}\0{}\0{:?}",
            self.command,
            self.script_path.as_deref().unwrap_or(""),
            self.interpreter
        );
        format!("shell-{:08x}", fnv1a(key.as_bytes()) as u32)
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let v = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let v = std::env::var_os("HOME");
    v.map(PathBuf::from)
}

/// macOS GUI apps inherit `/usr/bin:/bin:/usr/sbin:/sbin`; append the
/// Homebrew / `/usr/local` bin directories so user tools are found.
fn augmented_path(cur: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    if cfg!(not(target_os = "macos")) {
        return None;
    }
    let cur = cur.unwrap_or_default();
    let mut parts: Vec<PathBuf> = std::env::split_paths(&cur).collect();
    let mut changed = false;
    for extra in ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"] {
        let p = PathBuf::from(extra);
        if !parts.contains(&p) {
            parts.push(p);
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    std::env::join_paths(parts).ok()
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if (c == ' ' || c == '-' || c == '_' || c == '.')
            && !out.ends_with('-')
            && !out.is_empty()
        {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    out.chars().take(64).collect()
}

fn fnv1a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 10.0 {
        format!("{s:.1} s")
    } else {
        format!("{} s", s.round() as u64)
    }
}

/// `2026-10-26 14:03:07Z` (UTC).
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Capture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

type LogFile = Arc<Mutex<Option<File>>>;

/// A started shell action. [`wait`](Self::wait) blocks until it ended
/// (including the timeout); dropping it lets it finish in the background.
pub struct ShellRun {
    pub pid: u32,
    pub log: Option<PathBuf>,
    supervisor: Option<JoinHandle<ShellResult>>,
}

impl ShellRun {
    pub fn wait(mut self) -> ShellResult {
        let h = self.supervisor.take().expect("supervisor present");
        h.join().unwrap_or_else(|_| ShellResult {
            exit_code: None,
            signal: None,
            timed_out: false,
            stdout: String::new(),
            stderr: "supervisor panicked".into(),
            truncated: false,
            duration: Duration::ZERO,
            log: self.log.clone(),
        })
    }
}

fn open_log(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_ROTATE_BYTES) {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let _ = std::fs::rename(path, old);
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn log_line(log: &LogFile, line: &str) {
    if let Ok(mut g) = log.lock() {
        if let Some(f) = g.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// Start `action`. Returns once the process is running; the supervisor
/// thread enforces the timeout, collects output, writes the log footer and
/// notifies.
pub fn spawn(action: &ShellAction, ctx: &RunContext) -> Result<ShellRun, String> {
    let mut cmd = action.command()?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group, so a timeout can kill the whole tree.
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let log_path = if action.log_output {
        ctx.log_dir.as_deref().map(|d| action.log_path(d))
    } else {
        None
    };
    let log: LogFile = Arc::new(Mutex::new(None));
    if let Some(p) = &log_path {
        match open_log(p) {
            Ok(f) => *log.lock().unwrap() = Some(f),
            Err(e) => log::warn!("automation: log file {}: {e}", p.display()),
        }
    }
    let describe = {
        let prog = cmd.get_program().to_string_lossy().into_owned();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        format!("{prog} {}", args.join(" "))
    };

    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("could not start {}: {e}", action.display_name());
            log_line(&log, &format!("=== {} {msg}\n", timestamp()));
            return Err(msg);
        }
    };
    let pid = child.id();
    let tree = ProcessTree::new(&child);
    log_line(
        &log,
        &format!(
            "=== {} start pid {pid}: {}\n",
            timestamp(),
            describe.trim_end()
        ),
    );

    let cap = Arc::new(Mutex::new(Capture::default()));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let mut readers = 0;
    if let Some(out) = child.stdout.take() {
        spawn_reader(out, false, cap.clone(), log.clone(), done_tx.clone());
        readers += 1;
    }
    if let Some(err) = child.stderr.take() {
        spawn_reader(err, true, cap.clone(), log.clone(), done_tx.clone());
        readers += 1;
    }
    drop(done_tx);

    let (exit_tx, exit_rx) = mpsc::channel::<std::io::Result<ExitStatus>>();
    std::thread::Builder::new()
        .name("km-shell-wait".into())
        .spawn(move || {
            let _ = exit_tx.send(child.wait());
        })
        .map_err(|e| format!("thread: {e}"))?;

    let timeout = action.effective_timeout();
    let name = action.display_name();
    let notify = action.notify;
    let ctx = ctx.clone();
    let log_for_result = log_path.clone();
    let supervisor = std::thread::Builder::new()
        .name("km-shell".into())
        .spawn(move || {
            let mut timed_out = false;
            let status = match timeout {
                None => exit_rx.recv().ok(),
                Some(t) => match exit_rx.recv_timeout(t) {
                    Ok(s) => Some(s),
                    Err(_) => {
                        timed_out = true;
                        tree.terminate();
                        let s = exit_rx.recv_timeout(KILL_GRACE).ok();
                        tree.kill();
                        s.or_else(|| exit_rx.recv().ok())
                    }
                },
            };
            if timed_out {
                // Stragglers that ignored SIGTERM are gone after kill().
                tree.kill();
            }
            let duration = started.elapsed();
            // Let the readers drain; a background grandchild may keep the
            // pipes open, so don't wait for it forever.
            let deadline = Instant::now() + Duration::from_millis(500);
            for _ in 0..readers {
                let left = deadline.saturating_duration_since(Instant::now());
                if done_rx.recv_timeout(left).is_err() {
                    break;
                }
            }
            tree.close();
            let (exit_code, signal) = match status {
                Some(Ok(s)) => (s.code(), exit_signal(&s)),
                _ => (None, None),
            };
            let (stdout, stderr, truncated) = {
                let c = cap.lock().unwrap();
                (
                    String::from_utf8_lossy(&c.stdout).into_owned(),
                    String::from_utf8_lossy(&c.stderr).into_owned(),
                    c.truncated,
                )
            };
            let result = ShellResult {
                exit_code,
                signal,
                timed_out,
                stdout,
                stderr,
                truncated,
                duration,
                log: log_for_result,
            };
            log_line(
                &log,
                &format!(
                    "\n=== {} {}{} after {}\n",
                    timestamp(),
                    result.status_text(),
                    if timed_out {
                        " (process tree killed)"
                    } else {
                        ""
                    },
                    fmt_duration(duration)
                ),
            );
            if let Ok(mut g) = log.lock() {
                g.take();
            }
            let show = match notify {
                NotifyWhen::Never => false,
                NotifyWhen::Always => true,
                NotifyWhen::OnFailure => !result.success(),
            };
            if show {
                let (title, body) = notification_text(&name, &result);
                ctx.notify(&title, &body);
            }
            if !result.success() {
                log::info!("automation: '{name}' {}", result.status_text());
            }
            result
        })
        .map_err(|e| format!("thread: {e}"))?;
    Ok(ShellRun {
        pid,
        log: log_path,
        supervisor: Some(supervisor),
    })
}

/// Start and wait (the timeout still applies).
pub fn run_blocking(action: &ShellAction, ctx: &RunContext) -> Result<ShellResult, String> {
    Ok(spawn(action, ctx)?.wait())
}

/// Title and body of the finish notification.
pub fn notification_text(name: &str, r: &ShellResult) -> (String, String) {
    let title = format!("PowerMac: {name}");
    let mut body = if r.success() {
        "Finished (exit 0)".to_string()
    } else if r.timed_out {
        format!("Timed out after {}", fmt_duration(r.duration))
    } else {
        format!("Failed ({})", r.status_text())
    };
    if let Some(first) = r.first_line() {
        let mut l: String = first.chars().take(120).collect();
        if first.chars().count() > 120 {
            l.push('…');
        }
        body.push_str(" — ");
        body.push_str(&l);
    }
    (title, body)
}

fn spawn_reader(
    mut r: impl Read + Send + 'static,
    is_err: bool,
    cap: Arc<Mutex<Capture>>,
    log: LogFile,
    done: mpsc::Sender<()>,
) {
    let _ = std::thread::Builder::new()
        .name("km-shell-out".into())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match r.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut c) = cap.lock() {
                            let dst = if is_err { &mut c.stderr } else { &mut c.stdout };
                            let room = CAPTURE_LIMIT.saturating_sub(dst.len());
                            dst.extend_from_slice(&buf[..n.min(room)]);
                            if n > room {
                                c.truncated = true;
                            }
                        }
                        if let Ok(mut g) = log.lock() {
                            if let Some(f) = g.as_mut() {
                                let _ = f.write_all(&buf[..n]);
                            }
                        }
                    }
                }
            }
            let _ = done.send(());
        });
}

#[cfg(unix)]
fn exit_signal(s: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    s.signal()
}
#[cfg(not(unix))]
fn exit_signal(_: &ExitStatus) -> Option<i32> {
    None
}

/// The started process and everything it spawns: a process group on Unix,
/// a job object on Windows.
struct ProcessTree {
    #[cfg(unix)]
    pgid: i32,
    #[cfg(windows)]
    job: Option<isize>,
}

#[cfg(unix)]
impl ProcessTree {
    fn new(child: &std::process::Child) -> Self {
        Self {
            pgid: child.id() as i32,
        }
    }
    fn signal(&self, sig: i32) {
        if self.pgid > 1 {
            // SAFETY: plain syscall; a negative pid addresses the group.
            unsafe { libc::kill(-self.pgid, sig) };
        }
    }
    fn terminate(&self) {
        self.signal(libc::SIGTERM);
    }
    fn kill(&self) {
        self.signal(libc::SIGKILL);
    }
    fn close(&self) {}
}

#[cfg(windows)]
impl ProcessTree {
    fn new(child: &std::process::Child) -> Self {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        // SAFETY: plain Win32 calls on a handle we own / the child's handle.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Self { job: None };
            }
            // Processes the child started before this point escape the job
            // (std::process can't start a child suspended).
            if AssignProcessToJobObject(job, child.as_raw_handle() as _) == 0 {
                windows_sys::Win32::Foundation::CloseHandle(job);
                return Self { job: None };
            }
            Self {
                job: Some(job as isize),
            }
        }
    }
    fn terminate(&self) {
        if let Some(j) = self.job {
            // SAFETY: our job handle.
            unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(j as _, 1) };
        }
    }
    fn kill(&self) {
        self.terminate();
    }
    fn close(&self) {
        if let Some(j) = self.job {
            // SAFETY: our job handle, closed once (close() runs once).
            unsafe { windows_sys::Win32::Foundation::CloseHandle(j as _) };
        }
    }
}

#[cfg(not(any(unix, windows)))]
impl ProcessTree {
    fn new(_: &std::process::Child) -> Self {
        Self {}
    }
    fn terminate(&self) {}
    fn kill(&self) {}
    fn close(&self) {}
}

#[cfg(test)]
mod tests;
