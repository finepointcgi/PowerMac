//! Run one automation action the way a settings page's "Test" button does:
//! `cargo run -p keyboard-manager --example run_action -- '<action json>' [log dir]`
//! e.g. `'{"type":"shell","command":"echo hi > /tmp/x/out.txt"}'`.
//! Shell actions are waited for (test_action); per-action logs go to
//! `<log dir>/actions/` (default: the app's log folder).

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(json) = args.next() else {
        eprintln!("usage: run_action '<action json>' [log dir]");
        std::process::exit(2);
    };
    if let Some(dir) = args.next() {
        keyboard_manager::automation::set_log_dir(Some(dir.into()));
    }
    let action: keyboard_manager::automation::Action = match serde_json::from_str(&json) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("bad action: {e}");
            std::process::exit(2);
        }
    };
    for w in action.validate() {
        eprintln!("warning: {w}");
    }
    let o = keyboard_manager::test_action(&action);
    println!("{}", o.summary());
    if let keyboard_manager::automation::Outcome::Finished(r) = &o {
        println!(
            "exit={:?} signal={:?} timed_out={} duration={:?}",
            r.exit_code, r.signal, r.timed_out, r.duration
        );
        println!("stdout: {:?}", r.stdout);
        println!("stderr: {:?}", r.stderr);
        println!("log: {:?}", r.log);
    }
    // Give a notification's osascript a moment to be spawned and reaped.
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::process::exit(if o.is_ok() { 0 } else { 1 });
}
