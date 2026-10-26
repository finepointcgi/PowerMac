//! Keyboard Manager demo.
//!
//! ```text
//! cargo run -p keyboard-manager --example km_demo -- [OPTIONS] [config.json]
//!
//!   (no config)            use a built-in demo config (CapsLock -> Esc, ...)
//!   --powertoys FILE       import a PowerToys Keyboard Manager default.json
//!   --print-config         print the effective config as JSON and exit
//!   --app                  print the foreground app identifiers and exit
//!   --plain-clipboard      convert the clipboard to plain text (no paste) and exit
//!   --seconds N            run for N seconds (default 30), then stop
//! ```
//!
//! On macOS the hook needs Accessibility permission for the terminal running
//! this; without it the demo reports the error and exits with status 2.

use std::time::Duration;

use keyboard_manager::{clipboard, Config, KeyboardManager};

const DEMO: &str = r#"{
  "remapKeys": [
    { "from": "CapsLock", "to": "Esc" },
    { "from": "F13", "to": { "text": "Hello from PowerMac Keyboard Manager!" } }
  ],
  "remapShortcuts": [
    { "from": "Ctrl+Left",  "to": "Option+Left" },
    { "from": "Ctrl+Right", "to": "Option+Right" },
    { "from": "Ctrl+Shift+D", "to": { "text": "2026-01-01" } },
    { "from": "Ctrl+Q", "to": "disabled", "app": "com.apple.TextEdit" }
  ],
  "pastePlainText": { "enabled": true, "restoreClipboard": true }
}"#;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if let Err(e) = run() {
        eprintln!("km_demo: {e:#}");
        std::process::exit(2);
    }
}

fn run() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut config: Option<Config> = None;
    let mut seconds = 30u64;
    let mut print = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--powertoys" => {
                let path = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--powertoys needs a file"))?;
                let (c, warnings) = Config::from_powertoys_json(&std::fs::read_to_string(path)?)?;
                for w in warnings {
                    eprintln!("import warning: {w}");
                }
                config = Some(c);
            }
            "--print-config" => print = true,
            "--app" => {
                println!("{:?}", keyboard_manager::foreground_app());
                return Ok(());
            }
            "--plain-clipboard" => {
                match clipboard::convert_to_plain_text()? {
                    Some(t) => {
                        println!("clipboard is now plain text ({} chars)", t.chars().count())
                    }
                    None => println!("clipboard has no text"),
                }
                return Ok(());
            }
            "--seconds" => seconds = args.next().and_then(|s| s.parse().ok()).unwrap_or(seconds),
            path => config = Some(Config::from_json(&std::fs::read_to_string(path)?)?),
        }
    }
    let config = match config {
        Some(c) => c,
        None => Config::from_json(DEMO)?,
    };
    if print {
        println!("{}", config.to_json());
        return Ok(());
    }
    for w in config.validate() {
        eprintln!("config warning: {w}");
    }
    println!(
        "permission: {}",
        if keyboard_manager::has_permission(false) {
            "granted"
        } else {
            "missing (Accessibility)"
        }
    );
    println!("paste as plain text: {}", config.paste_plain_text.hotkey);
    let km = KeyboardManager::start(config)?;
    println!("running for {seconds}s; try the remaps now");
    std::thread::sleep(Duration::from_secs(seconds));
    km.stop();
    println!("stopped");
    Ok(())
}
