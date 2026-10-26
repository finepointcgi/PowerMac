//! Manual checks:
//! * `cargo run -p color-picker --example probe hotkey` registers and
//!   unregisters the default hotkey (prints whether that worked).
//! * `... probe pick` shows the picker and prints the color (Esc cancels).
//! * `... probe pick-timeout <secs>` exits after <secs> even without a pick.
//! * `... probe editor <data-dir>` opens the editor.

fn main() -> anyhow::Result<()> {
    env_logger_lite();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hotkey") => {
            let dir = std::env::temp_dir().join("color-picker-probe");
            let h =
                color_picker::ColorPicker::start(color_picker::Config::default(), &dir, || {
                    println!("open editor")
                })?;
            println!("registered {}", color_picker::Config::default().hotkey);
            drop(h);
            println!("unregistered");
        }
        Some("pick") => println!("{:?}", color_picker::pick_once()),
        Some("pick-timeout") => {
            let secs: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2.0);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs_f64(secs));
                println!("timeout: sampler was shown, exiting");
                std::process::exit(0);
            });
            println!("{:?}", color_picker::pick_once());
        }
        Some("editor") => {
            let dir = args
                .get(1)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("color-picker-probe"));
            color_picker::run_editor(dir, color_picker::Config::default())?;
        }
        _ => eprintln!("usage: probe hotkey | pick | pick-timeout <secs> | editor [dir]"),
    }
    Ok(())
}

fn env_logger_lite() {
    struct L;
    impl log::Log for L {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, r: &log::Record) {
            eprintln!("[{}] {}", r.level(), r.args());
        }
        fn flush(&self) {}
    }
    let _ = log::set_boxed_logger(Box::new(L)).map(|_| log::set_max_level(log::LevelFilter::Info));
}
