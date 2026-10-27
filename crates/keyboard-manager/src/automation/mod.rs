//! The action set shared by automation triggers: Keyboard Manager hotkeys
//! ([`crate::Config::hotkeys`]), mouse buttons (`mouse-tuning`'s
//! `ButtonAction`) and trackpad gestures (`gestures`).
//!
//! An [`Action`] is JSON with a `"type"` tag (camelCase):
//!
//! | JSON | does |
//! |---|---|
//! | `{"type": "disabled"}` | nothing (e.g. turn a global binding off in one app) |
//! | `{"type": "shortcut", "keys": "Cmd+Shift+T"}` | press a key / key combination |
//! | `{"type": "text", "text": "hello"}` | type text |
//! | `{"type": "run", "path": "/Applications/Safari.app", "args": [], "workingDir": null}` | start a program |
//! | `{"type": "openUri", "uri": "https://example.com"}` | open a URL / file / folder |
//! | `{"type": "pastePlainText"}` | paste the clipboard as plain text |
//! | `{"type": "shell", "command": "say hi", ...}` | a [`ShellAction`] |
//!
//! Only commands the user configured are ever run: actions come from the
//! user's config (or the settings page's test button) and nothing here
//! builds a command from other input.
//!
//! Run an action with [`run_action_once`] (what a trigger does) or
//! [`test_action`] (a settings "Test" button: also waits for background
//! shell actions so their output can be shown), or queue it on an
//! [`Executor`] from an input-hook callback.

use std::path::PathBuf;
use std::sync::{mpsc, Mutex, OnceLock, RwLock};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

use crate::config::RunProgram;
use crate::keys::{concrete, is_modifier, parse_combo};

pub mod shell;

pub use shell::{
    Interpreter, Notifier, NotifyWhen, RunContext, ShellAction, ShellResult, ShellRun,
};

/// Called with each action an [`Executor`] ran and its outcome.
pub type OnDone = Box<dyn Fn(&Action, &Outcome) + Send>;

/// Something a hotkey, mouse button or gesture does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    /// Do nothing (swallow the trigger).
    Disabled,
    /// Press a key or key combination (`"Cmd+Shift+T"`, `"F5"`), see
    /// [`crate::keys::parse_combo`].
    Shortcut { keys: String },
    /// Type text (`\n` presses Return).
    Text { text: String },
    /// Start a program.
    Run(RunProgram),
    /// Open a URL, file or folder.
    OpenUri { uri: String },
    /// Paste the clipboard as plain text (default paste settings).
    PastePlainText,
    /// Run a shell command / script.
    Shell(ShellAction),
}

impl Action {
    pub fn label(&self) -> &'static str {
        match self {
            Action::Disabled => "Do nothing",
            Action::Shortcut { .. } => "Keyboard shortcut",
            Action::Text { .. } => "Type text",
            Action::Run(_) => "Open app / program",
            Action::OpenUri { .. } => "Open URL / file",
            Action::PastePlainText => "Paste as plain text",
            Action::Shell(_) => "Shell command",
        }
    }

    /// One empty instance of every kind, for a settings drop-down.
    pub fn all() -> Vec<Action> {
        vec![
            Action::Disabled,
            Action::Shortcut {
                keys: String::new(),
            },
            Action::Text {
                text: String::new(),
            },
            Action::Run(RunProgram {
                path: String::new(),
                args: Vec::new(),
                working_dir: None,
            }),
            Action::OpenUri { uri: String::new() },
            Action::PastePlainText,
            Action::Shell(ShellAction::default()),
        ]
    }

    /// Configuration problems (empty = fine).
    pub fn validate(&self) -> Vec<String> {
        match self {
            Action::Shortcut { keys } => match parse_combo(keys) {
                Ok(_) => Vec::new(),
                Err(e) => vec![e],
            },
            Action::Run(r) if r.path.trim().is_empty() => vec!["no program path".into()],
            Action::OpenUri { uri } if uri.trim().is_empty() => vec!["no URL".into()],
            Action::Shell(s) => s.validate(),
            _ => Vec::new(),
        }
    }

    /// True for actions that inject keyboard input (need Accessibility on
    /// macOS).
    pub fn injects_input(&self) -> bool {
        matches!(
            self,
            Action::Shortcut { .. } | Action::Text { .. } | Action::PastePlainText
        )
    }
}

/// What running an action did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Performed (keys sent, program / URL opened, paste done).
    Done,
    /// [`Action::Disabled`]: nothing to do.
    Skipped,
    /// A background shell action is running.
    Started { pid: u32, log: Option<PathBuf> },
    /// A shell action that was waited for ended (successfully or not).
    Finished(ShellResult),
    /// It could not be performed.
    Failed(String),
}

impl Outcome {
    /// Done / skipped / started, or finished with exit status 0.
    pub fn is_ok(&self) -> bool {
        match self {
            Outcome::Done | Outcome::Skipped | Outcome::Started { .. } => true,
            Outcome::Finished(r) => r.success(),
            Outcome::Failed(_) => false,
        }
    }

    /// One line for a settings page.
    pub fn summary(&self) -> String {
        match self {
            Outcome::Done => "Done".into(),
            Outcome::Skipped => "Nothing to do".into(),
            Outcome::Started { pid, .. } => format!("Started in the background (pid {pid})"),
            Outcome::Finished(r) => match r.first_line() {
                Some(l) => format!("{}: {l}", r.status_text()),
                None => r.status_text(),
            },
            Outcome::Failed(e) => format!("Failed: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Global context (log folder, notifier)
// ---------------------------------------------------------------------------

struct Globals {
    log_dir: Option<PathBuf>,
    notifier: Option<Notifier>,
}

fn globals() -> &'static RwLock<Globals> {
    static G: OnceLock<RwLock<Globals>> = OnceLock::new();
    G.get_or_init(|| {
        RwLock::new(Globals {
            log_dir: default_log_dir(),
            notifier: None,
        })
    })
}

/// The app's log folder: `~/Library/Logs/PowerMac` on macOS,
/// `%LOCALAPPDATA%\PowerMac` on Windows (as the app's own log).
pub fn default_log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    return std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Logs/PowerMac"));
    #[cfg(windows)]
    return std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("PowerMac"));
    #[cfg(not(any(target_os = "macos", windows)))]
    return std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/powermac"));
}

/// Override the log folder (per-action logs go to `<dir>/actions/`);
/// `None` turns log files off.
pub fn set_log_dir(dir: Option<PathBuf>) {
    if let Ok(mut g) = globals().write() {
        g.log_dir = dir;
    }
}

/// Route finish notifications through the app (default: [`shell::default_notify`]).
pub fn set_notifier(f: Option<Notifier>) {
    if let Ok(mut g) = globals().write() {
        g.notifier = f;
    }
}

/// The context [`run_action_once`] uses.
pub fn run_context() -> RunContext {
    match globals().read() {
        Ok(g) => RunContext {
            log_dir: g.log_dir.clone(),
            notifier: g.notifier.clone(),
        },
        Err(_) => RunContext::default(),
    }
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

/// Run `action` as configured: background shell actions return
/// [`Outcome::Started`] immediately, waited ones block until they finish
/// (or time out). Blocks briefly for key injection; call it off the input
/// hook (see [`Executor`]).
pub fn run_action_once(action: &Action) -> Outcome {
    run_with(action, &run_context(), false)
}

/// Like [`run_action_once`] but shell actions are always waited for (the
/// configured timeout, or 30 s), so a settings page can show the result.
pub fn test_action(action: &Action) -> Outcome {
    run_with(action, &run_context(), true)
}

/// [`run_action_once`] with an explicit context; `force_wait` as in
/// [`test_action`].
pub fn run_with(action: &Action, ctx: &RunContext, force_wait: bool) -> Outcome {
    let r: anyhow::Result<Outcome> = match action {
        Action::Disabled => Ok(Outcome::Skipped),
        Action::Shortcut { keys } => send_keys(keys).map(|_| Outcome::Done),
        Action::Text { text } => {
            crate::platform::inject(&[crate::engine::Action::Text(text.clone())])
                .map(|_| Outcome::Done)
        }
        Action::Run(r) => crate::launch::run_program(r).map(|_| Outcome::Done),
        Action::OpenUri { uri } => crate::launch::open_uri(uri).map(|_| Outcome::Done),
        Action::PastePlainText => {
            crate::paste_plain_text_with(&crate::PastePlainText::default()).map(|_| Outcome::Done)
        }
        Action::Shell(s) => {
            return run_shell(s, ctx, force_wait);
        }
    };
    r.unwrap_or_else(|e| Outcome::Failed(format!("{e:#}")))
}

/// Run a shell action (as configured, or waited for with `force_wait`).
pub fn run_shell(s: &ShellAction, ctx: &RunContext, force_wait: bool) -> Outcome {
    let mut s = s.clone();
    if force_wait && !s.wait {
        // Keep the configured timeout; a background action without one gets
        // the waiting default.
        if s.timeout_ms.is_none() {
            s.timeout_ms = Some(shell::DEFAULT_WAIT_TIMEOUT.as_millis() as u64);
        }
        s.wait = true;
    }
    match shell::spawn(&s, ctx) {
        Err(e) => Outcome::Failed(e),
        Ok(run) if s.wait => Outcome::Finished(run.wait()),
        Ok(run) => Outcome::Started {
            pid: run.pid,
            log: run.log.clone(),
        },
    }
}

/// The key events for `"Cmd+Shift+T"`: modifiers down, key down/up,
/// modifiers up (reverse order). A lone modifier is pressed and released.
pub fn key_events(keys: &str) -> Result<Vec<crate::engine::Action>, String> {
    use crate::engine::Action as K;
    let vks: Vec<u32> = parse_combo(keys)?.into_iter().map(concrete).collect();
    let (mods, rest): (Vec<u32>, Vec<u32>) = vks.iter().partition(|&&k| is_modifier(k));
    let (mods, key) = match rest.as_slice() {
        [] => {
            let (last, m) = mods.split_last().ok_or("empty key combination")?;
            (m.to_vec(), *last)
        }
        [k] => (mods, *k),
        _ => return Err(format!("'{keys}' has more than one non-modifier key")),
    };
    let mut out: Vec<K> = mods.iter().map(|&m| K::down(m)).collect();
    out.push(K::down(key));
    out.push(K::up(key));
    out.extend(mods.iter().rev().map(|&m| K::up(m)));
    Ok(out)
}

fn send_keys(keys: &str) -> anyhow::Result<()> {
    let ev = key_events(keys).map_err(anyhow::Error::msg)?;
    crate::platform::inject(&ev)
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// A worker thread that runs queued actions in order (waited shell actions
/// hold up the ones behind them). Use from input-hook callbacks: `submit`
/// never blocks.
pub struct Executor {
    tx: Mutex<Option<mpsc::Sender<Action>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Executor {
    /// Start the worker; `on_done` sees each action's outcome (logging,
    /// a settings page's "last result").
    pub fn start(name: &str, on_done: Option<OnDone>) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Action>();
        let label = name.to_string();
        let thread = std::thread::Builder::new()
            .name(format!("{name}-actions"))
            .spawn(move || {
                for a in rx {
                    let o = run_action_once(&a);
                    if !o.is_ok() {
                        log::warn!("{label}: {}: {}", a.label(), o.summary());
                    }
                    if let Some(f) = &on_done {
                        f(&a, &o);
                    }
                }
            })?;
        Ok(Self {
            tx: Mutex::new(Some(tx)),
            thread: Mutex::new(Some(thread)),
        })
    }

    /// Queue an action (dropped after [`stop`](Self::stop)).
    pub fn submit(&self, a: Action) {
        if let Ok(g) = self.tx.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(a);
            }
        }
    }

    /// A sender for another thread (keep none alive past `stop`, which
    /// waits for the queue to drain).
    pub fn sender(&self) -> Option<mpsc::Sender<Action>> {
        self.tx.lock().ok().and_then(|g| g.clone())
    }

    /// Finish the queued actions and end the worker. Background shell
    /// actions keep running.
    pub fn stop(&self) {
        if let Ok(mut g) = self.tx.lock() {
            g.take();
        }
        if let Some(t) = self.thread.lock().ok().and_then(|mut g| g.take()) {
            let _ = t.join();
        }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// App scoping (shared by gesture / button bindings)
// ---------------------------------------------------------------------------

/// Whether a binding scoped to `app` (`None` = every app) applies when the
/// frontmost app has these identifiers (see [`crate::foreground_app`]).
/// Case-insensitive; `.app` / `.exe` suffixes and directories are ignored.
pub fn app_matches(app: Option<&str>, front: &[String]) -> bool {
    match app.map(str::trim).filter(|a| !a.is_empty()) {
        None => true,
        Some(a) => {
            let want = crate::engine::normalize_app(a);
            front
                .iter()
                .any(|f| crate::engine::normalize_app(f) == want)
        }
    }
}

/// The best binding among `candidates` (already filtered to the trigger):
/// the first one scoped to the frontmost app, else the first global one.
pub fn resolve_scoped<'a, T>(
    candidates: impl IntoIterator<Item = &'a T>,
    app_of: impl Fn(&T) -> Option<&str>,
    front: &[String],
) -> Option<&'a T> {
    let mut global = None;
    for c in candidates {
        match app_of(c).map(str::trim).filter(|a| !a.is_empty()) {
            Some(_) if app_matches(app_of(c), front) => return Some(c),
            Some(_) => {}
            None => {
                if global.is_none() {
                    global = Some(c);
                }
            }
        }
    }
    global
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_input::vk::*;

    #[test]
    fn action_json_round_trip() {
        let json = r#"[
          {"type": "disabled"},
          {"type": "shortcut", "keys": "Cmd+Shift+T"},
          {"type": "text", "text": "hi\n"},
          {"type": "run", "path": "/Applications/Safari.app", "args": ["-x"]},
          {"type": "openUri", "uri": "https://example.com"},
          {"type": "pastePlainText"},
          {"type": "shell", "command": "echo hi", "interpreter": "bash", "env": {"A": "1"},
           "wait": true, "timeoutMs": 5000, "notify": "onFailure", "workingDir": "/tmp"},
          {"type": "shell", "scriptPath": "~/bin/x.py", "interpreter": "python3", "notify": true, "logOutput": false},
          {"type": "shell", "command": "x", "interpreter": {"custom": {"program": "/opt/homebrew/bin/fish", "args": ["-c"]}}}
        ]"#;
        let v: Vec<Action> = serde_json::from_str(json).unwrap();
        assert_eq!(v.len(), 9);
        assert_eq!(
            v[1],
            Action::Shortcut {
                keys: "Cmd+Shift+T".into()
            }
        );
        let Action::Shell(s) = &v[6] else { panic!() };
        assert_eq!(s.interpreter, Interpreter::Bash);
        assert!(s.wait && s.log_output);
        assert_eq!(s.timeout_ms, Some(5000));
        assert_eq!(s.notify, NotifyWhen::OnFailure);
        assert_eq!(s.env.get("A").map(String::as_str), Some("1"));
        let Action::Shell(s) = &v[7] else { panic!() };
        assert_eq!(s.notify, NotifyWhen::Always);
        assert!(!s.log_output && !s.wait);
        assert_eq!(s.script_path.as_deref(), Some("~/bin/x.py"));
        let Action::Shell(s) = &v[8] else { panic!() };
        assert!(
            matches!(&s.interpreter, Interpreter::Custom { program, args } if program.ends_with("fish") && args == &["-c"])
        );
        // Round trip, and the compact form omits defaults.
        let back: Vec<Action> = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(back, v);
        let min = serde_json::to_value(Action::Shell(ShellAction {
            command: "ls".into(),
            ..Default::default()
        }))
        .unwrap();
        assert_eq!(min["type"], "shell");
        assert_eq!(min["command"], "ls");
        assert_eq!(min["interpreter"], "zsh");
        assert!(min.get("env").is_none() && min.get("timeoutMs").is_none());
        for a in Action::all() {
            let s = serde_json::to_string(&a).unwrap();
            assert_eq!(serde_json::from_str::<Action>(&s).unwrap(), a, "{s}");
            assert!(!a.label().is_empty());
        }
        assert!(serde_json::from_str::<Action>(r#"{"type":"nope"}"#).is_err());
        assert!(
            serde_json::from_str::<Action>(r#"{"type":"shell","notify":"sometimes"}"#).is_err()
        );
    }

    #[test]
    fn validation() {
        assert!(
            Action::Shortcut {
                keys: "Cmd+Nope".into()
            }
            .validate()
            .len()
                == 1
        );
        assert!(Action::Shortcut {
            keys: "Cmd+W".into()
        }
        .validate()
        .is_empty());
        let s = |c: &str, p: Option<&str>| {
            Action::Shell(ShellAction {
                command: c.into(),
                script_path: p.map(Into::into),
                ..Default::default()
            })
        };
        assert!(s("", None)
            .validate()
            .iter()
            .any(|e| e.contains("no command")));
        assert!(s("x", Some("/y"))
            .validate()
            .iter()
            .any(|e| e.contains("not both")));
        assert!(s("", Some("/definitely/missing.sh"))
            .validate()
            .iter()
            .any(|e| e.contains("not found")));
        assert!(s("true", None).validate().is_empty());
    }

    #[test]
    fn key_event_sequences() {
        use crate::engine::Action as K;
        assert_eq!(
            key_events("Cmd+Shift+T").unwrap(),
            vec![
                K::down(VK_LWIN),
                K::down(VK_LSHIFT),
                K::down(b'T' as u32),
                K::up(b'T' as u32),
                K::up(VK_LSHIFT),
                K::up(VK_LWIN)
            ]
        );
        assert_eq!(
            key_events("F5").unwrap(),
            vec![K::down(VK_F5), K::up(VK_F5)]
        );
        assert_eq!(
            key_events("Ctrl").unwrap(),
            vec![K::down(VK_LCONTROL), K::up(VK_LCONTROL)]
        );
        assert!(key_events("A+B").is_err());
        assert!(key_events("").is_err());
    }

    #[test]
    fn scoped_resolution() {
        struct B(Option<&'static str>, u32);
        let bs = [
            B(None, 1),
            B(Some("com.apple.Safari"), 2),
            B(Some("Terminal.app"), 3),
            B(None, 4),
        ];
        let r = |front: &[&str]| {
            let f: Vec<String> = front.iter().map(|s| s.to_string()).collect();
            resolve_scoped(bs.iter(), |b| b.0, &f).map(|b| b.1)
        };
        assert_eq!(r(&[]), Some(1));
        assert_eq!(r(&["com.apple.safari", "Safari"]), Some(2));
        assert_eq!(r(&["com.apple.Terminal", "Terminal"]), Some(3));
        assert_eq!(r(&["Finder"]), Some(1));
        assert_eq!(
            resolve_scoped(bs[1..3].iter(), |b| b.0, &["x".to_string()]).map(|b| b.1),
            None
        );
        assert!(app_matches(None, &[]));
        assert!(app_matches(Some("  "), &[]));
        assert!(app_matches(Some("CHROME.EXE"), &["chrome.exe".into()]));
    }

    #[test]
    fn disabled_and_failed_outcomes() {
        assert_eq!(run_action_once(&Action::Disabled), Outcome::Skipped);
        let o = run_with(
            &Action::Shell(ShellAction::default()),
            &RunContext::default(),
            false,
        );
        assert!(
            matches!(&o, Outcome::Failed(e) if e.contains("no command")),
            "{o:?}"
        );
        assert!(!o.is_ok());
        assert!(o.summary().starts_with("Failed"));
    }
}
