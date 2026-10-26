//! PowerMac: Mouse Without Borders, FancyZones and Awake in one cross-platform app.

mod clipboard;
mod modules;
mod tray;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use pm_config::AppConfig;

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
    /// Run every enabled module with a tray icon (default).
    Run {
        /// Don't show the menu-bar / notification-area icon.
        #[arg(long)]
        no_tray: bool,
    },
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
    /// Open the settings window (edits config.json and fancyzones.json; a running
    /// PowerMac applies the changes within about a second).
    Settings,
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
pub(crate) struct Running {
    pub(crate) mwb: Option<mwb::EngineHandle>,
    pub(crate) zones: Option<fancyzones::Handle>,
    pub(crate) awake: Option<awake::Awake>,
    pub(crate) scroll: Option<scroll_reverser::Handle>,
    pub(crate) mouse: Option<mouse_utils::Handle>,
    pub(crate) keyboard: Option<keyboard_manager::Handle>,
}

/// The config file in use (for the tray's "copy key" / "open config folder").
pub(crate) static CONFIG_PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Change the config file in use; the config watcher then applies it.
pub(crate) fn edit_config(f: impl FnOnce(&mut AppConfig)) -> Result<()> {
    let path = CONFIG_PATH.get().cloned().or_else(AppConfig::default_path).context("no config path")?;
    pm_config::edit_at(&path, f)
}

pub(crate) static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

pub(crate) fn shutdown() {
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
        if let Some(s) = &mut r.scroll {
            s.stop();
        }
        if let Some(m) = &mut r.mouse {
            m.stop();
        }
        if let Some(k) = r.keyboard.take() {
            k.stop();
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
    match cli.cmd.unwrap_or(Cmd::Run { no_tray: false }) {
        Cmd::Run { no_tray } => run(&path, !no_tray),
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
        Cmd::Settings => settings_ui::run(path),
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

fn run(path: &std::path::Path, tray: bool) -> Result<()> {
    // First launch (e.g. opening PowerMac.app): create a config with a fresh key.
    if !path.exists() {
        AppConfig::with_new_key().save(path)?;
        log::info!("created {} with a new Mouse Without Borders key", path.display());
    }
    let cfg = AppConfig::load_or_default(path)?;
    let _ = CONFIG_PATH.set(path.to_path_buf());
    let mut running = Running::default();
    modules::apply(&mut running, None, &cfg);
    modules::watch(path, cfg.clone());

    let anything = running.mwb.is_some() || running.zones.is_some() || cfg.awake.enabled;
    if !anything && !tray {
        bail!("no module is running (check the config and permissions with `powermac doctor`)");
    }
    *RUNNING.lock().unwrap() = Some(running);
    install_ctrlc();

    #[cfg(target_os = "macos")]
    {
        if tray {
            // NSStatusItem needs the shared NSApplication to exist first.
            let mtm = objc2::MainThreadMarker::new().context("run must be on the main thread")?;
            let _ = objc2_app_kit::NSApplication::sharedApplication(mtm);
            tray::install()?;
        }
        // The FancyZones overlay and the tray are AppKit UI: run the app loop here.
        fancyzones::platform::run_main_loop();
    }

    #[cfg(windows)]
    {
        if tray {
            tray::install()?;
        }
        win_message_loop();
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = tray;
        let (_tx, rx) = std::sync::mpsc::channel::<()>();
        let _ = rx.recv();
    }
    #[allow(unreachable_code)]
    {
        shutdown();
        Ok(())
    }
}

/// Pump the Win32 message loop (the tray icon's hidden window lives on this
/// thread) and refresh the tray menu every 2 seconds.
#[cfg(windows)]
fn win_message_loop() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, SetTimer, TranslateMessage, MSG, WM_TIMER};
    unsafe {
        SetTimer(std::ptr::null_mut(), 0, 2000, None);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.message == WM_TIMER && msg.hwnd.is_null() {
                tray::refresh_tick();
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
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
