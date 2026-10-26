//! "Run program" and "open URI" targets.

use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::config::RunProgram;

/// Expand `~/` (Unix) or `%VAR%` (Windows) in a path.
pub(crate) fn expand_path(p: &str) -> String {
    #[cfg(windows)]
    {
        let mut out = String::new();
        let mut rest = p;
        while let Some(i) = rest.find('%') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            match after.find('%') {
                Some(j) if j > 0 => {
                    let name = &after[..j];
                    match std::env::var(name) {
                        Ok(v) => out.push_str(&v),
                        Err(_) => {
                            out.push('%');
                            out.push_str(name);
                            out.push('%');
                        }
                    }
                    rest = &after[j + 1..];
                }
                _ => {
                    out.push('%');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }
    #[cfg(not(windows))]
    {
        match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => format!("{}/{rest}", home.to_string_lossy()),
            _ => p.to_string(),
        }
    }
}

fn spawn_detached(mut cmd: Command) -> Result<()> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    // Reap it so it doesn't linger as a zombie (Unix); harmless on Windows.
    std::thread::Builder::new()
        .name("km-child-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

/// Build the command for a run-program target (exposed for tests).
pub(crate) fn program_command(r: &RunProgram) -> Command {
    let path = expand_path(&r.path);
    #[cfg(target_os = "macos")]
    let mut cmd = if path.trim_end_matches('/').ends_with(".app") {
        let mut c = Command::new("/usr/bin/open");
        c.arg("-a").arg(&path);
        if !r.args.is_empty() {
            c.arg("--args").args(&r.args);
        }
        c
    } else {
        let mut c = Command::new(&path);
        c.args(&r.args);
        c
    };
    #[cfg(not(target_os = "macos"))]
    let mut cmd = {
        let mut c = Command::new(&path);
        c.args(&r.args);
        c
    };
    if let Some(wd) = r.working_dir.as_deref().filter(|w| !w.is_empty()) {
        cmd.current_dir(expand_path(wd));
    } else if let Some(parent) = std::path::Path::new(&path).parent().filter(|p| p.is_dir()) {
        // PowerToys starts programs in their own directory by default.
        if !cfg!(target_os = "macos") || !path.ends_with(".app") {
            cmd.current_dir(parent);
        }
    }
    cmd
}

pub fn run_program(r: &RunProgram) -> Result<()> {
    spawn_detached(program_command(r)).with_context(|| format!("starting {}", r.path))
}

pub fn open_uri(uri: &str) -> Result<()> {
    let target = expand_path(uri);
    #[cfg(target_os = "macos")]
    {
        let mut c = Command::new("/usr/bin/open");
        c.arg(&target);
        spawn_detached(c).with_context(|| format!("opening {uri}"))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let wide = |s: &str| {
            s.encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let (op, file) = (wide("open"), wide(&target));
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // Values <= 32 are errors.
        if (r as isize) <= 32 {
            anyhow::bail!("ShellExecuteW({uri}) failed: {}", r as isize);
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let mut c = Command::new("xdg-open");
        c.arg(&target);
        spawn_detached(c).with_context(|| format!("opening {uri}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_shapes() {
        let r = RunProgram {
            path: "/usr/bin/true".into(),
            args: vec!["a b".into()],
            working_dir: None,
        };
        let c = program_command(&r);
        assert_eq!(c.get_program(), "/usr/bin/true");
        assert_eq!(c.get_args().collect::<Vec<_>>(), vec!["a b"]);
        #[cfg(target_os = "macos")]
        {
            let r = RunProgram {
                path: "/Applications/Safari.app".into(),
                args: vec!["-x".into()],
                working_dir: None,
            };
            let c = program_command(&r);
            assert_eq!(c.get_program(), "/usr/bin/open");
            assert_eq!(
                c.get_args().collect::<Vec<_>>(),
                vec!["-a", "/Applications/Safari.app", "--args", "-x"]
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_true() {
        run_program(&RunProgram {
            path: "/usr/bin/true".into(),
            args: vec![],
            working_dir: Some("/".into()),
        })
        .unwrap();
        assert!(run_program(&RunProgram {
            path: "/nonexistent/x".into(),
            args: vec![],
            working_dir: None
        })
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn tilde() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_path("~/x"), format!("{home}/x"));
        assert_eq!(expand_path("/a/~/b"), "/a/~/b");
    }
}
