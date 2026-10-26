//! PowerMac: Mouse Without Borders, FancyZones and Awake in one cross-platform app.

mod config;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use config::AppConfig;

#[derive(Parser)]
#[command(name = "powermac", version, about = "PowerToys-style utilities for macOS and Windows")]
struct Cli {
    /// Path to config.json (default: <config dir>/PowerMac/config.json).
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run every enabled module (default).
    Run,
    /// Write a default config file with a freshly generated MWB key.
    Init {
        /// Overwrite an existing config file.
        #[arg(long)]
        force: bool,
    },
    /// Print a new Mouse Without Borders security key.
    GenKey,
    /// Show config location, permissions and screens.
    Doctor {
        /// Ask the OS for missing permissions (macOS shows a prompt).
        #[arg(long)]
        prompt: bool,
    },
    /// Keep the computer awake (standalone, like PowerToys Awake's CLI).
    Awake {
        /// Keep the display on too.
        #[arg(short, long)]
        display_on: bool,
        /// Stay awake for this many seconds.
        #[arg(short, long)]
        time_limit: Option<u64>,
        /// Stay awake until this time (RFC 3339, "YYYY-MM-DD HH:MM" or "HH:MM").
        #[arg(short, long)]
        expire_at: Option<String>,
        /// Stay awake while this process is alive.
        #[arg(short, long)]
        pid: Option<u32>,
    },
}

/// Running modules, kept globally so the Ctrl+C handler can shut them down
/// (restoring the cursor and releasing power assertions) before exiting.
#[derive(Default)]
struct Running {
    mwb: Option<mwb::EngineHandle>,
    zones: Option<fancyzones::Handle>,
    awake: Option<awake::Awake>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

fn shutdown() {
    if let Some(mut r) = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take() {
        if let Some(m) = &r.mwb {
            m.stop();
        }
        if let Some(z) = &mut r.zones {
            z.stop();
        }
        if let Some(a) = &r.awake {
            a.stop();
        }
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let path = match cli.config {
        Some(p) => p,
        None => AppConfig::default_path().context("no config directory on this platform")?,
    };
    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => run(&path),
        Cmd::Init { force } => {
            if path.exists() && !force {
                bail!("{} already exists (use --force to overwrite)", path.display());
            }
            let cfg = AppConfig::with_new_key();
            cfg.save(&path)?;
            println!("wrote {}", path.display());
            println!("MWB security key: {}", cfg.mwb.settings.key);
            println!("Enter this key (and this machine's name, {}) on your other machines.",
                cfg.mwb.settings.effective_machine_name());
            Ok(())
        }
        Cmd::GenKey => {
            println!("{}", mwb::create_random_key());
            Ok(())
        }
        Cmd::Doctor { prompt } => doctor(&path, prompt),
        Cmd::Awake { display_on, time_limit, expire_at, pid } => {
            let mode = match (time_limit, expire_at) {
                (Some(_), Some(_)) => bail!("use either --time-limit or --expire-at"),
                (Some(s), None) => awake::AwakeMode::Timed(Duration::from_secs(s)),
                (None, Some(e)) => awake::AwakeMode::Expirable(awake::parse_expire_at(&e, chrono::Local::now())?),
                (None, None) => awake::AwakeMode::Indefinite,
            };
            let a = awake::Awake::new()?;
            a.set(awake::AwakeConfig { mode, keep_display_on: display_on, bound_pid: pid })?;
            println!("awake: {:?}{}", mode, if display_on { " (display on)" } else { "" });
            *RUNNING.lock().unwrap() = Some(Running { awake: Some(a), ..Default::default() });
            install_ctrlc();
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let guard = RUNNING.lock().unwrap();
                let Some(st) = guard.as_ref().and_then(|r| r.awake.as_ref()).map(|a| a.status()) else { break };
                if !st.active {
                    println!("awake ended: {:?}", st.last_end);
                    break;
                }
            }
            shutdown();
            Ok(())
        }
    }
}

fn install_ctrlc() {
    let _ = ctrlc::set_handler(|| {
        log::info!("shutting down");
        shutdown();
        std::process::exit(0);
    });
}

fn run(path: &std::path::Path) -> Result<()> {
    let cfg = AppConfig::load_or_default(path)?;
    if !path.exists() {
        log::warn!("no config at {} — run `powermac init` first; using defaults", path.display());
    }
    let mut running = Running::default();

    if cfg.awake.enabled {
        let a = awake::Awake::new()?;
        a.set(cfg.awake.settings)?;
        log::info!("awake: {:?}", cfg.awake.settings.mode);
        running.awake = Some(a);
    }

    if cfg.mwb.enabled {
        let backend = pm_input::backend()?;
        if !backend.has_permissions(true) {
            log::error!("mouse without borders needs Accessibility + Input Monitoring permission; \
                         grant it in System Settings → Privacy & Security and restart");
        } else if !mwb::crypto::is_key_valid(&cfg.mwb.settings.key) {
            log::error!("mouse without borders: key missing or shorter than 16 chars (run `powermac init`)");
        } else {
            let h = mwb::Engine::start(cfg.mwb.settings.clone(), backend)?;
            log::info!("mouse without borders: {} listening on {}", h.machine_name(), h.local_addr());
            if let Some(rx) = h.events() {
                std::thread::spawn(move || {
                    for ev in rx {
                        log::info!("mwb: {ev:?}");
                    }
                });
            }
            running.mwb = Some(h);
        }
    }

    if cfg.fancyzones.enabled {
        let fz_cfg = fancyzones::Config::load_or_default()?;
        match fancyzones::FancyZones::start(fz_cfg) {
            Ok(h) => {
                log::info!("fancyzones: running");
                running.zones = Some(h);
            }
            Err(e) => log::error!("fancyzones: {e:#}"),
        }
    }

    if running.mwb.is_none() && running.zones.is_none() && running.awake.is_none() {
        bail!("no module is running (check the config and permissions with `powermac doctor`)");
    }
    *RUNNING.lock().unwrap() = Some(running);
    install_ctrlc();

    // The FancyZones overlay is AppKit UI, so on macOS the main thread runs the app loop.
    #[cfg(target_os = "macos")]
    fancyzones::platform::run_main_loop();

    #[cfg(not(target_os = "macos"))]
    {
        let (_tx, rx) = std::sync::mpsc::channel::<()>();
        let _ = rx.recv();
    }
    #[allow(unreachable_code)]
    {
        shutdown();
        Ok(())
    }
}

fn doctor(path: &std::path::Path, prompt: bool) -> Result<()> {
    println!("config: {} ({})", path.display(), if path.exists() { "exists" } else { "missing" });
    let cfg = AppConfig::load_or_default(path)?;
    println!("modules: mwb={} fancyzones={} awake={}",
        cfg.mwb.enabled, cfg.fancyzones.enabled, cfg.awake.enabled);
    println!("machine name: {}", cfg.mwb.settings.effective_machine_name());
    println!("mwb key valid: {}", mwb::crypto::is_key_valid(&cfg.mwb.settings.key));
    let backend: Arc<dyn pm_input::InputBackend> = pm_input::backend()?;
    println!("input permissions: {}", backend.has_permissions(prompt));
    for (i, s) in backend.screens()?.iter().enumerate() {
        println!("screen {i}: {}x{} at ({}, {})", s.width(), s.height(), s.left, s.top);
    }
    Ok(())
}
