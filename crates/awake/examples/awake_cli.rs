//! Command-line front end similar to PowerToys `PowerToys.Awake.exe`.
//!
//! ```text
//! awake_cli [--display-on [true|false]] [--time-limit <secs>]
//!           [--expire-at <RFC3339 | YYYY-MM-DD HH:MM | HH:MM>]
//!           [--pid <pid>] [--use-parent-pid] [--config <json>]
//! ```
//! With no mode flag it stays awake indefinitely (like PowerToys). The process
//! exits when the mode reverts to passive (timer/expiry/pid exit); Ctrl+C also
//! releases the assertion because the OS drops it with the process.

use std::time::Duration;

use awake::{parse_expire_at, Awake, AwakeConfig, AwakeMode};

fn usage() -> ! {
    eprintln!(
        "usage: awake_cli [-d|--display-on [true|false]] [-t|--time-limit SECS]\n\
         \x20                [-e|--expire-at RFC3339|'YYYY-MM-DD HH:MM'|HH:MM] [-p|--pid PID]\n\
         \x20                [-u|--use-parent-pid] [--config JSON]"
    );
    std::process::exit(2)
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut display = false;
    let mut time_limit: Option<u64> = None;
    let mut expire_at: Option<String> = None;
    let mut pid: Option<u32> = None;
    let mut config_json: Option<String> = None;

    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().unwrap_or_else(|| {
            eprintln!("{name} needs a value");
            usage()
        });
        match a.as_str() {
            "-d" | "--display-on" => {
                display = true;
                // Optional explicit bool, as in PowerToys (`--display-on false`).
                if let Some(next) = args.peek() {
                    match next.to_ascii_lowercase().as_str() {
                        "true" => { args.next(); }
                        "false" => { display = false; args.next(); }
                        _ => {}
                    }
                }
            }
            "-t" | "--time-limit" => {
                time_limit = Some(val(&a).parse().unwrap_or_else(|_| {
                    eprintln!("--time-limit must be a number of seconds");
                    usage()
                }))
            }
            "-e" | "--expire-at" => expire_at = Some(val(&a)),
            "-p" | "--pid" => {
                pid = Some(val(&a).parse().unwrap_or_else(|_| {
                    eprintln!("--pid must be a process id");
                    usage()
                }))
            }
            "-u" | "--use-parent-pid" => pid = Some(parent_pid()),
            "--config" => config_json = Some(val(&a)),
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown argument {other:?}");
                usage()
            }
        }
    }

    let config = if let Some(j) = config_json {
        serde_json::from_str::<AwakeConfig>(&j).unwrap_or_else(|e| {
            eprintln!("bad --config JSON: {e}");
            std::process::exit(2)
        })
    } else {
        let mode = if let Some(s) = expire_at {
            match parse_expire_at(&s, chrono::Local::now()) {
                Ok(t) => AwakeMode::Expirable(t),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2)
                }
            }
        } else if let Some(secs) = time_limit {
            AwakeMode::Timed(Duration::from_secs(secs))
        } else {
            AwakeMode::Indefinite
        };
        AwakeConfig { mode, keep_display_on: display, bound_pid: pid }
    };

    let awake = Awake::new().unwrap_or_else(|e| {
        eprintln!("failed to start: {e}");
        std::process::exit(1)
    });
    if let Err(e) = awake.set(config) {
        eprintln!("failed to set mode: {e}");
        std::process::exit(1);
    }
    println!("awake: {}", serde_json::to_string(&awake.status()).unwrap());

    loop {
        std::thread::sleep(Duration::from_secs(1));
        let st = awake.status();
        if !st.active {
            println!("awake: reverted to passive ({:?})", st.last_end);
            break;
        }
        if let Some(r) = st.remaining {
            println!("awake: {:?} remaining", Duration::from_secs(r.as_secs()));
        }
    }
}

#[cfg(unix)]
fn parent_pid() -> u32 {
    std::os::unix::process::parent_id()
}

#[cfg(not(unix))]
fn parent_pid() -> u32 {
    eprintln!("--use-parent-pid is only supported on Unix in this example");
    std::process::exit(2)
}
