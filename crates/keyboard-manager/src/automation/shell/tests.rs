use super::*;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "km-shell-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn sh(cmd: &str) -> ShellAction {
        ShellAction {
            command: cmd.into(),
            interpreter: Interpreter::Sh,
            ..Default::default()
        }
    }

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks for existence.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    type Seen = Arc<Mutex<Vec<(String, String)>>>;

    fn ctx(dir: &Path) -> (RunContext, Seen) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let c = RunContext {
            log_dir: Some(dir.to_path_buf()),
            notifier: Some(Arc::new(move |t: &str, b: &str| {
                s2.lock().unwrap().push((t.into(), b.into()))
            })),
        };
        (c, seen)
    }

    #[test]
    fn wait_captures_output_exit_code_and_logs() {
        let d = tmp();
        let (c, seen) = ctx(&d);
        let a = ShellAction {
            name: "Capture Test".into(),
            notify: NotifyWhen::Always,
            ..sh("echo first line; echo second; echo oops >&2; exit 3")
        };
        let r = run_blocking(&a, &c).unwrap();
        assert_eq!(r.exit_code, Some(3));
        assert!(!r.success() && !r.timed_out);
        assert_eq!(r.stdout, "first line\nsecond\n");
        assert_eq!(r.stderr, "oops\n");
        assert_eq!(r.first_line(), Some("first line"));
        assert_eq!(r.status_text(), "exit 3");
        let log = r.log.clone().unwrap();
        assert_eq!(log, d.join("actions/capture-test.log"));
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains("start pid") && text.contains("first line") && text.contains("oops"),
            "{text}"
        );
        assert!(text.contains("exit 3 after"), "{text}");
        let n = seen.lock().unwrap().clone();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].0, "PowerMac: Capture Test");
        assert!(
            n[0].1.starts_with("Failed (exit 3) — first line"),
            "{:?}",
            n[0]
        );

        // A second run appends to the same log.
        let r2 = run_blocking(&a, &c).unwrap();
        assert_eq!(r2.log, Some(log.clone()));
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap()
                .matches("start pid")
                .count(),
            2
        );

        // onFailure stays quiet on success; no log when logOutput is off.
        let ok = ShellAction {
            notify: NotifyWhen::OnFailure,
            log_output: false,
            ..sh("echo fine")
        };
        let r = run_blocking(&ok, &c).unwrap();
        assert!(r.success() && r.log.is_none());
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "only the two runs of `a` notified"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn env_and_working_dir() {
        let d = tmp();
        let (c, _) = ctx(&d);
        let mut a = sh("pwd; echo \"$FOO|$POWERMAC_ACTION|$1\"");
        a.working_dir = Some(d.to_string_lossy().into_owned());
        a.env.insert("FOO".into(), "bar baz".into());
        a.args = vec!["arg1".into()];
        a.name = "envtest".into();
        let r = run_blocking(&a, &c).unwrap();
        assert!(r.success(), "{r:?}");
        let lines: Vec<&str> = r.stdout.lines().collect();
        assert_eq!(PathBuf::from(lines[0]).canonicalize().unwrap(), d);
        assert_eq!(lines[1], "bar baz|envtest|arg1");

        // Default working dir is $HOME.
        let r = run_blocking(&sh("pwd"), &RunContext::default()).unwrap();
        let home = PathBuf::from(std::env::var("HOME").unwrap())
            .canonicalize()
            .unwrap();
        assert_eq!(PathBuf::from(r.stdout.trim()).canonicalize().unwrap(), home);

        // Missing working dir fails to start.
        let mut bad = sh("true");
        bad.working_dir = Some("/definitely/not/here".into());
        assert!(spawn(&bad, &c).err().unwrap().contains("working directory"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn script_path_interpreters() {
        let d = tmp();
        let (c, _) = ctx(&d);
        let script = d.join("s.sh");
        std::fs::write(&script, "echo \"script $1 $2\"\n").unwrap();
        for interp in [Interpreter::Sh, Interpreter::Bash, Interpreter::Zsh] {
            let a = ShellAction {
                script_path: Some(script.to_string_lossy().into_owned()),
                args: vec!["a".into(), "b c".into()],
                interpreter: interp.clone(),
                ..Default::default()
            };
            let r = run_blocking(&a, &c).unwrap();
            assert_eq!(r.stdout, "script a b c\n", "{interp:?} {r:?}");
        }
        // Custom interpreter: program + args + command.
        let a = ShellAction {
            command: "echo custom".into(),
            interpreter: Interpreter::Custom {
                program: "/bin/sh".into(),
                args: vec!["-c".into()],
            },
            ..Default::default()
        };
        assert_eq!(run_blocking(&a, &c).unwrap().stdout, "custom\n");
        #[cfg(target_os = "macos")]
        {
            let a = ShellAction {
                command: "return \"from applescript\"".into(),
                interpreter: Interpreter::Osascript,
                ..Default::default()
            };
            let r = run_blocking(&a, &c).unwrap();
            assert_eq!(r.stdout.trim(), "from applescript", "{r:?}");
        }
        if Path::new("/usr/bin/python3").exists()
            && Command::new("/usr/bin/python3")
                .arg("-c")
                .arg("1")
                .output()
                .is_ok_and(|o| o.status.success())
        {
            let a = ShellAction {
                command: "import sys, os; print('py', sys.argv[1], os.environ['K'])".into(),
                interpreter: Interpreter::Python3,
                args: vec!["x".into()],
                env: [("K".to_string(), "v".to_string())].into(),
                ..Default::default()
            };
            assert_eq!(run_blocking(&a, &c).unwrap().stdout, "py x v\n");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn background_returns_immediately_wait_blocks() {
        let d = tmp();
        let (c, seen) = ctx(&d);
        let marker = d.join("done");
        let cmd = format!("sleep 0.6; echo done > '{}'", marker.display());

        // Background: run_shell returns Started at once; the file appears later.
        let t = Instant::now();
        let o = crate::automation::run_shell(
            &ShellAction {
                notify: NotifyWhen::Always,
                name: "bg".into(),
                ..sh(&cmd)
            },
            &c,
            false,
        );
        assert!(
            t.elapsed() < Duration::from_millis(400),
            "{:?}",
            t.elapsed()
        );
        assert!(
            matches!(o, crate::automation::Outcome::Started { .. }),
            "{o:?}"
        );
        assert!(!marker.exists());
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(marker.exists());
        assert_eq!(seen.lock().unwrap()[0].1, "Finished (exit 0)");
        std::fs::remove_file(&marker).unwrap();

        // Wait: blocks until the command is done.
        let t = Instant::now();
        let o = crate::automation::run_shell(
            &ShellAction {
                wait: true,
                ..sh(&cmd)
            },
            &c,
            false,
        );
        assert!(t.elapsed() >= Duration::from_millis(550));
        assert!(marker.exists());
        let crate::automation::Outcome::Finished(r) = o else {
            panic!("{o:?}")
        };
        assert!(r.success());

        // force_wait (test button) waits for a background action too.
        std::fs::remove_file(&marker).unwrap();
        let o = crate::automation::run_shell(&sh(&cmd), &c, true);
        assert!(
            matches!(o, crate::automation::Outcome::Finished(ref r) if r.success()),
            "{o:?}"
        );
        assert!(marker.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn timeout_kills_process_tree() {
        let d = tmp();
        let (c, seen) = ctx(&d);
        let pidfile = d.join("pids");
        // A grandchild in the background, and a TERM-ignoring parent.
        let cmd = format!(
            "sleep 30 & echo $! > '{p}'; sh -c 'trap \"\" TERM; sleep 30' & echo $! >> '{p}'; echo started; wait",
            p = pidfile.display()
        );
        let a = ShellAction {
            timeout_ms: Some(400),
            wait: true,
            notify: NotifyWhen::OnFailure,
            name: "slow".into(),
            ..sh(&cmd)
        };
        let t = Instant::now();
        let r = run_blocking(&a, &c).unwrap();
        let took = t.elapsed();
        assert!(r.timed_out, "{r:?}");
        assert!(!r.success());
        assert!(took < Duration::from_secs(5), "{took:?}");
        assert_eq!(r.first_line(), Some("started"));
        let pids: Vec<i32> = std::fs::read_to_string(&pidfile)
            .unwrap()
            .lines()
            .map(|l| l.trim().parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 2);
        let deadline = Instant::now() + Duration::from_secs(3);
        while pids.iter().any(|&p| alive(p)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        for p in &pids {
            assert!(!alive(*p), "pid {p} survived the timeout");
        }
        let n = seen.lock().unwrap().clone();
        assert!(n[0].1.starts_with("Timed out after"), "{n:?}");
        let log = std::fs::read_to_string(r.log.unwrap()).unwrap();
        assert!(log.contains("process tree killed"), "{log}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn background_grandchild_holding_pipes_does_not_hang() {
        let d = tmp();
        let (c, _) = ctx(&d);
        let pidfile = d.join("pid");
        let cmd = format!(
            "sleep 20 & echo $! > '{}'; echo parent done",
            pidfile.display()
        );
        let t = Instant::now();
        let r = run_blocking(
            &ShellAction {
                wait: true,
                ..sh(&cmd)
            },
            &c,
        )
        .unwrap();
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert!(r.success());
        assert_eq!(r.stdout, "parent done\n");
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Only a timeout kills the tree; clean up ourselves.
        assert!(alive(pid));
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn start_failure() {
        let d = tmp();
        let (c, _) = ctx(&d);
        let a = ShellAction {
            command: "x".into(),
            interpreter: Interpreter::Custom {
                program: "/nonexistent/interp".into(),
                args: vec![],
            },
            name: "broken".into(),
            ..Default::default()
        };
        let e = spawn(&a, &c).err().unwrap();
        assert!(e.contains("could not start broken"), "{e}");
        let log = std::fs::read_to_string(a.log_path(&d)).unwrap();
        assert!(log.contains("could not start"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn output_capture_is_bounded() {
        let d = tmp();
        let (c, _) = ctx(&d);
        let a = ShellAction {
            log_output: false,
            ..sh("head -c 400000 /dev/zero | tr '\\0' 'x'")
        };
        let r = run_blocking(&a, &c).unwrap();
        assert!(r.success());
        assert_eq!(r.stdout.len(), CAPTURE_LIMIT);
        assert!(r.truncated);
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[test]
fn command_shapes() {
    let a = ShellAction {
        command: "echo hi".into(),
        args: vec!["x".into()],
        ..Default::default()
    };
    let c = a.command().unwrap();
    let args: Vec<_> = c
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    if cfg!(unix) {
        assert_eq!(c.get_program(), "/bin/zsh");
    }
    assert_eq!(args, ["-c", "echo hi", "powermac-action", "x"]);
    let a = ShellAction {
        script_path: Some("/x/y.py".into()),
        interpreter: Interpreter::Python3,
        ..Default::default()
    };
    let c = a.command().unwrap();
    let args: Vec<_> = c
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    if cfg!(unix) {
        assert_eq!(args, ["python3", "/x/y.py"]);
    }
    let a = ShellAction {
        command: "Get-Date".into(),
        interpreter: Interpreter::Powershell,
        ..Default::default()
    };
    let args: Vec<_> = a
        .command()
        .unwrap()
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        ["-NoProfile", "-NonInteractive", "-Command", "Get-Date"]
    );
    if cfg!(not(windows)) {
        let a = ShellAction {
            command: "dir".into(),
            interpreter: Interpreter::Cmd,
            ..Default::default()
        };
        assert!(a.command().is_err());
        assert!(!a.validate().is_empty());
    }
}

#[test]
fn timeouts_and_names() {
    let mut a = ShellAction {
        command: "true".into(),
        ..Default::default()
    };
    assert_eq!(a.effective_timeout(), None);
    a.wait = true;
    assert_eq!(a.effective_timeout(), Some(DEFAULT_WAIT_TIMEOUT));
    a.timeout_ms = Some(0);
    assert_eq!(a.effective_timeout(), None);
    a.timeout_ms = Some(1500);
    assert_eq!(a.effective_timeout(), Some(Duration::from_millis(1500)));
    assert_eq!(a.display_name(), "true");
    assert!(a
        .log_path(Path::new("/l"))
        .to_string_lossy()
        .contains("/l/actions/shell-"));
    a.name = "  My Script: v2!  ".into();
    assert_eq!(a.display_name(), "My Script: v2!");
    assert_eq!(
        a.log_path(Path::new("/l")),
        PathBuf::from("/l/actions/my-script-v2.log")
    );
    let long = ShellAction {
        command: "x".repeat(100),
        ..Default::default()
    };
    assert_eq!(long.display_name().chars().count(), 41);
    let s = ShellAction {
        script_path: Some("~/bin/backup.sh".into()),
        ..Default::default()
    };
    assert_eq!(s.display_name(), "backup.sh");
}

#[test]
fn timestamp_format() {
    let t = timestamp();
    assert_eq!(t.len(), 20, "{t}");
    assert!(t.starts_with("20") && t.ends_with('Z'));
}

#[test]
fn notification_texts() {
    let r = ShellResult {
        exit_code: Some(0),
        signal: None,
        timed_out: false,
        stdout: "\n  hello world  \nmore".into(),
        stderr: String::new(),
        truncated: false,
        duration: Duration::from_millis(1200),
        log: None,
    };
    assert_eq!(
        notification_text("A", &r),
        (
            "PowerMac: A".into(),
            "Finished (exit 0) — hello world".into()
        )
    );
    let r = ShellResult {
        exit_code: None,
        timed_out: true,
        stdout: String::new(),
        duration: Duration::from_secs(12),
        ..r
    };
    assert_eq!(notification_text("A", &r).1, "Timed out after 12 s");
    let r = ShellResult {
        timed_out: false,
        signal: Some(9),
        stderr: "boom".into(),
        ..r
    };
    assert_eq!(
        notification_text("A", &r).1,
        "Failed (killed by signal 9) — boom"
    );
}

#[test]
fn notify_accepts_bools_and_strings() {
    for (j, w) in [
        ("true", NotifyWhen::Always),
        ("false", NotifyWhen::Never),
        ("\"onFailure\"", NotifyWhen::OnFailure),
        ("\"on-failure\"", NotifyWhen::OnFailure),
        ("\"always\"", NotifyWhen::Always),
    ] {
        assert_eq!(serde_json::from_str::<NotifyWhen>(j).unwrap(), w, "{j}");
    }
    assert_eq!(
        serde_json::to_string(&NotifyWhen::OnFailure).unwrap(),
        "\"onFailure\""
    );
}
