//! Manual checks (read-only unless noted): `cargo run -p workspaces --example live -- <cmd>`
//! with `capture` (JSON of the desktop), `monitors`, `windows [bundle id]`, `watch <bundle id>`,
//! `launch <data dir> <name>` (moves windows!), `close <doc prefix>`, `import <data dir> <file>`, `editor <data dir>`.

fn main() -> anyhow::Result<()> {
    env_logger_init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = workspaces::Config::default();
    cfg.launch.restore_spaces = std::env::var_os("WS_SPACES").is_some();
    match args.first().map(String::as_str) {
        Some("monitors") => println!("{}", serde_json::to_string_pretty(&workspaces::platform::monitors()?)?),
        Some("capture") => {
            let t = std::time::Instant::now();
            let ws = workspaces::capture("Live", &cfg)?;
            eprintln!("captured {} windows in {:?}", ws.apps.len(), t.elapsed());
            println!("{}", serde_json::to_string_pretty(&ws)?);
        }
        Some("launch") => {
            let dir = std::path::PathBuf::from(args.get(1).expect("data dir"));
            let name = args.get(2).expect("name");
            let r = workspaces::launch(&dir, name, &cfg, &|e| {
                eprintln!("[{}] {} -> {:?} {}", e.index, e.app, e.status, e.detail.clone().unwrap_or_default())
            })?;
            eprintln!("{} in {:?}", r.summary(), r.elapsed);
        }
        Some("windows") => {
            use workspaces::backend::Backend;
            let n = workspaces::platform::Native::new(&cfg.capture);
            for w in n.windows()? {
                if args.get(1).is_none_or(|b| *b == w.app.bundle_id) {
                    println!("{} pid={} {:?} {:?} {:?} doc={:?} space={:?} {}", w.id, w.pid, w.frame, w.state, w.app.name, w.document, w.space, w.title);
                }
            }
        }
        #[cfg(target_os = "macos")]
        Some("close") => {
            use workspaces::backend::Backend;
            let prefix = args.get(1).expect("document prefix");
            let n = workspaces::platform::Native::new(&cfg.capture);
            for w in n.windows()? {
                if w.document.as_deref().is_some_and(|d| d.starts_with(prefix.as_str())) {
                    println!("closing {} {:?}", w.id, w.document);
                    n.close_window_for_tests(&w)?;
                }
            }
        }
        Some("watch") => {
            use workspaces::backend::Backend;
            let n = workspaces::platform::Native::new(&cfg.capture);
            let bid = args.get(1).expect("bundle id").clone();
            std::process::Command::new("/usr/bin/open").args(["-g", "-b", &bid]).status()?;
            for i in 0..20 {
                let ws: Vec<String> = n.windows()?.into_iter().filter(|w| w.app.bundle_id.eq_ignore_ascii_case(&bid)).map(|w| format!("{} {:?} {:?}", w.id, w.frame, w.state)).collect();
                println!("{i}: {ws:?}");
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
        Some("import") => {
            let dir = std::path::PathBuf::from(args.get(1).expect("data dir"));
            let r = workspaces::import_powertoys_file(&dir, std::path::Path::new(args.get(2).expect("file")))?;
            println!("{}", r.summary());
            for w in &r.workspaces {
                for a in &w.apps {
                    println!("  {} -> {} {} {:?} on {}", w.name, a.name, a.bundle_id, a.frame, a.monitor_id);
                }
            }
        }
        Some("editor") => workspaces::run_editor(std::path::PathBuf::from(args.get(1).expect("data dir")))?,
        _ => eprintln!("usage: live capture | monitors | launch <data dir> <name> | editor <data dir>"),
    }
    Ok(())
}

fn env_logger_init() {
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            if r.target().starts_with("workspaces") {
                eprintln!("{} {}", r.level(), r.args());
            }
        }
        fn flush(&self) {}
    }
    let _ = log::set_logger(&L).map(|()| log::set_max_level(log::LevelFilter::Info));
}
