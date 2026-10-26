//! Running user-configured commands (Script widget, xbar plugins, item
//! actions, Shortcuts) with a time limit and an output cap.
//!
//! Security: only commands and plugin files the user entered in the
//! widget settings are ever run; nothing from fetched content (web
//! responses) is executed, and script / plugin item actions only run what
//! the user's own command printed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::catalog::Interpreter;

/// Most output kept from a run.
pub const MAX_OUTPUT: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RunOutput {
    pub stdout: String,
    pub stderr: String,
    /// Exit code (`None` when killed by a signal / the time limit).
    pub code: Option<i32>,
    pub timed_out: bool,
}

impl RunOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    /// A short message for a failed run: the last stderr line, or the exit
    /// status.
    pub fn error_message(&self) -> String {
        if self.timed_out {
            return "Timed out".into();
        }
        let last = self.stderr.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        match (self.code, last.is_empty()) {
            (Some(c), true) => format!("Exited with status {c}"),
            (_, false) => last.chars().take(200).collect(),
            (None, true) => "Stopped by a signal".into(),
        }
    }
}

/// The command line for running `command` with `interp`.
pub fn argv_for(interp: Interpreter, custom: &str, command: &str) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).chain([command.to_string()]).collect::<Vec<String>>();
    match interp {
        Interpreter::Zsh => s(&["/bin/zsh", "-lc"]),
        Interpreter::Bash => s(&["/bin/bash", "-lc"]),
        Interpreter::Sh => s(&["/bin/sh", "-c"]),
        Interpreter::Python3 => s(&["/usr/bin/env", "python3", "-c"]),
        Interpreter::Node => s(&["/bin/zsh", "-lc", "exec node -e \"$0\""]),
        Interpreter::Ruby => s(&["/usr/bin/env", "ruby", "-e"]),
        Interpreter::Perl => s(&["/usr/bin/perl", "-e"]),
        Interpreter::Osascript => s(&["/usr/bin/osascript", "-e"]),
        Interpreter::Custom => {
            let p = super::xbar::expand_tilde(custom);
            vec![p.to_string_lossy().into_owned(), "-c".into(), command.to_string()]
        }
    }
}

/// How plugins are started: executable files directly, others through
/// their interpreter by extension.
pub fn plugin_argv(path: &Path) -> Vec<String> {
    let p = path.to_string_lossy().into_owned();
    if super::xbar::is_executable(path) {
        return vec![p];
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let interp: &[&str] = match ext.as_str() {
        "sh" | "zsh" => &["/bin/zsh"],
        "bash" => &["/bin/bash"],
        "py" => &["/usr/bin/env", "python3"],
        "rb" => &["/usr/bin/env", "ruby"],
        "js" => &["/usr/bin/env", "node"],
        "pl" => &["/usr/bin/perl"],
        "swift" => &["/usr/bin/env", "swift"],
        _ => &["/bin/zsh"],
    };
    interp.iter().map(|s| s.to_string()).chain([p]).collect()
}

/// Run `argv` (no shell of its own) with a time limit. The process gets
/// its own process group, which is killed on timeout so children don't
/// linger.
pub fn run(argv: &[String], cwd: Option<&Path>, env: &[(String, String)], timeout: Duration) -> Result<RunOutput, String> {
    let (prog, args) = argv.split_first().ok_or("Nothing to run")?;
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(d) = cwd.filter(|d| d.is_dir()) {
        cmd.current_dir(d);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start {prog}: {e}"))?;
    let read = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.by_ref().take(MAX_OUTPUT as u64).read_to_end(&mut buf);
                // Drain (and drop) the rest so the child doesn't block.
                let _ = std::io::copy(&mut r, &mut std::io::sink());
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out_t = read(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err_t = read(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            kill_group(&mut child);
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(if start.elapsed() < Duration::from_millis(200) { 5 } else { 25 }));
    };
    // The readers end when every writer (grandchildren too) is gone; after
    // a kill that is immediate. Don't wait forever on a daemonized child.
    let join = |t: std::thread::JoinHandle<String>| {
        let deadline = Instant::now() + Duration::from_millis(500);
        while !t.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if t.is_finished() {
            t.join().unwrap_or_default()
        } else {
            String::new()
        }
    };
    let stdout = join(out_t);
    let stderr = join(err_t);
    Ok(RunOutput { stdout, stderr, code: status.and_then(|s| s.code()), timed_out })
}

fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pgid = child.id();
        let _ = Command::new("/bin/kill").args(["-9", "--", &format!("-{pgid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = child.kill();
}

/// Start a command and forget it (item actions). Output is discarded.
pub fn spawn_detached(argv: &[String], cwd: Option<&Path>) -> Result<(), String> {
    let (prog, args) = argv.split_first().ok_or("Nothing to run")?;
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(d) = cwd.filter(|d| d.is_dir()) {
        cmd.current_dir(d);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start {prog}: {e}"))?;
    // Reap it in the background.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// A `.command` file that runs `argv` in Terminal (opened with `open`, so
/// no Automation permission is needed). Returns its path.
pub fn terminal_script(dir: &Path, argv: &[String]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let quoted: Vec<String> = argv.iter().map(|a| shell_quote(a)).collect();
    let body = format!("#!/bin/zsh -l\n{}\necho\necho '[Process finished — press any key to close]'\nread -k 1\n", quoted.join(" "));
    let name = format!("powermac-run-{}.command", super::content::hash_bytes(body.as_bytes()));
    let p = dir.join(name);
    std::fs::write(&p, body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(p)
}

/// Single-quote for zsh / sh.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:,@%+".contains(&b)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(cmd: &str) -> Vec<String> {
        argv_for(Interpreter::Sh, "", cmd)
    }

    #[test]
    fn runs_and_captures() {
        let o = run(&sh("echo hello; echo oops >&2"), None, &[], Duration::from_secs(5)).unwrap();
        assert!(o.ok());
        assert_eq!(o.stdout, "hello\n");
        assert_eq!(o.stderr, "oops\n");
        let o = run(&sh("echo bad >&2; exit 3"), None, &[], Duration::from_secs(5)).unwrap();
        assert_eq!((o.code, o.error_message().as_str()), (Some(3), "bad"));
        let o = run(&sh("exit 4"), None, &[], Duration::from_secs(5)).unwrap();
        assert_eq!(o.error_message(), "Exited with status 4");
        let o = run(&sh("echo $PM_TEST; pwd"), Some(Path::new("/")), &[("PM_TEST".into(), "v1".into())], Duration::from_secs(5)).unwrap();
        assert_eq!(o.stdout, "v1\n/\n");
        assert!(run(&["/nonexistent/prog".to_string()], None, &[], Duration::from_secs(1)).is_err());
        assert!(run(&[], None, &[], Duration::from_secs(1)).is_err());
    }

    #[test]
    fn times_out_and_kills_children() {
        let t0 = Instant::now();
        let o = run(&sh("sleep 30 & sleep 30; echo never"), None, &[], Duration::from_millis(300)).unwrap();
        assert!(o.timed_out && !o.ok());
        assert_eq!(o.error_message(), "Timed out");
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    }

    #[test]
    fn caps_output() {
        let o = run(&sh("yes 0123456789 | head -c 1000000"), None, &[], Duration::from_secs(10)).unwrap();
        assert_eq!(o.stdout.len(), MAX_OUTPUT);
        assert!(o.ok());
    }

    #[test]
    fn argv_shapes() {
        assert_eq!(argv_for(Interpreter::Zsh, "", "date"), vec!["/bin/zsh", "-lc", "date"]);
        assert_eq!(argv_for(Interpreter::Python3, "", "print(1)"), vec!["/usr/bin/env", "python3", "-c", "print(1)"]);
        assert_eq!(argv_for(Interpreter::Custom, "/opt/fish", "x"), vec!["/opt/fish", "-c", "x"]);
        let o = run(&argv_for(Interpreter::Perl, "", "print 6*7"), None, &[], Duration::from_secs(5)).unwrap();
        assert_eq!(o.stdout, "42");
        assert_eq!(shell_quote("plain-arg"), "plain-arg");
        assert_eq!(shell_quote("it's here"), "'it'\\''s here'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(plugin_argv(Path::new("/nonexistent/x.5m.py")), vec!["/usr/bin/env", "python3", "/nonexistent/x.5m.py"]);
    }

    #[test]
    fn terminal_scripts() {
        let dir = std::env::temp_dir().join(format!("ns-term-{}", std::process::id()));
        let p = terminal_script(&dir, &["/bin/echo".into(), "a b".into()]).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("/bin/echo 'a b'"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
