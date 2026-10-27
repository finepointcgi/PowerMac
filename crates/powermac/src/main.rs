//! PowerMac: PowerToys-style utilities (Mouse Without Borders, FancyZones,
//! Awake, Keyboard Manager, Color Picker, ...) in one cross-platform app.

mod app_glue;
mod audio_glue;
mod children;
mod clipboard;
mod display_glue;
mod menubar_glue;
mod modules;
mod mwb_glue;
mod net_cli;
mod notch_glue;
mod quick;
mod scaling_glue;
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
        /// Open the settings window on this page once running (used when
        /// PowerMac restarts itself from the settings window).
        #[arg(long, hide = true)]
        open_settings: Option<String>,
        /// Wait for this process (the previous instance) to exit first.
        #[arg(long, hide = true)]
        wait_pid: Option<u32>,
    },
    /// Write a default config file with a freshly generated MWB key.
    Init {
        /// Overwrite an existing config file.
        #[arg(long)]
        force: bool,
    },
    /// Print a new Mouse Without Borders security key.
    GenKey,
    /// Mouse Without Borders tools (`powermac mwb doctor`).
    Mwb {
        #[command(subcommand)]
        cmd: MwbCmd,
    },
    /// Saved desktops of apps and windows: `list`, `capture [NAME]`,
    /// `launch NAME|ID`, `edit` (the editor window).
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// Network monitor and per-app firewall rules: `list`, `watch`, `audit`,
    /// `rules list|add|remove|enable|disable|default|import|check`.
    Net {
        #[command(subcommand)]
        cmd: net_cli::NetCmd,
    },
    /// Show the privacy permissions this PowerMac has (no prompts).
    Permissions {
        /// Print JSON (what the settings window reads).
        #[arg(long)]
        json: bool,
    },
    /// Check GitHub for a newer PowerMac (one request; nothing is installed).
    Update {
        #[command(subcommand)]
        cmd: UpdateCmd,
    },
    /// Show config location, permissions and screens.
    Doctor {
        /// Ask the OS for missing permissions (macOS shows a prompt).
        #[arg(long)]
        prompt: bool,
    },
    /// Open the settings window (edits config.json and fancyzones.json; a running
    /// PowerMac applies the changes within about a second).
    Settings {
        /// Open on this page ("notch-widgets", "sound", "notch-shelf", ...;
        /// the page's title also works).
        #[arg(long)]
        page: Option<String>,
    },
    /// Open the FancyZones layout editor (edits fancyzones.json; a running
    /// PowerMac applies the changes within about a second).
    Editor {
        /// Monitor id to start on (default: the main monitor).
        #[arg(long)]
        monitor: Option<String>,
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
    /// Open the clipboard history picker (the hotkey and the tray start it).
    Clipboard,
    /// Pick a color from the screen, copy it in the configured format and
    /// print it (or open the Color Picker editor with --editor).
    ColorPicker {
        /// Open the editor (history, variations, every format) instead.
        #[arg(long)]
        editor: bool,
    },
    /// Recognize text: in an image file, in the clipboard image, or (with
    /// neither) in a region you select on screen. Prints the text; a selected
    /// region is also copied to the clipboard.
    Ocr {
        /// An image file (PNG, JPEG, HEIC, TIFF, ...).
        file: Option<std::path::PathBuf>,
        /// Use the image on the clipboard.
        #[arg(long, conflicts_with = "file")]
        clipboard: bool,
    },
    /// Bulk-rename files and folders with PowerRename (the Finder Quick
    /// Action runs this).
    Rename {
        #[arg(required = true)]
        paths: Vec<std::path::PathBuf>,
    },
    /// Create a new file or folder from a template (New+) in a folder (the
    /// Finder Quick Action runs this).
    New {
        #[arg(long)]
        dir: std::path::PathBuf,
    },
    /// Resize pictures with Image Resizer: opens its window, or resizes
    /// right away with a size option (see `powermac resize --help`; the
    /// Finder Quick Action runs this).
    #[command(disable_help_flag = true)]
    Resize {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Finder basics: open-terminal, open-editor, copy-path, show-hidden /
    /// hide-hidden / toggle-hidden, volumes, eject-all, who-uses, apps (run
    /// `powermac finder` for the details; the Quick Actions run this).
    #[command(disable_help_flag = true)]
    Finder {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// List the saved workspaces.
    List,
    /// Save the windows on screen as a new workspace ("Workspace N" when
    /// no name is given).
    Capture { name: Option<String> },
    /// Restore a workspace by name or id.
    Launch { name: String },
    /// Open the Workspaces editor.
    Edit,
}

#[derive(Subcommand)]
enum UpdateCmd {
    /// Ask GitHub Releases now and print the result.
    Check {
        /// Consider pre-releases too.
        #[arg(long)]
        pre: bool,
    },
}

#[derive(Subcommand)]
enum MwbCmd {
    /// Check everything that decides whether a Windows PC can pair with this
    /// Mac (name, ports, key, LLMNR, VPN, firewall, recent connection
    /// attempts) and print the PowerShell to run on the PC.
    Doctor,
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
    pub(crate) mouse_tuning: Option<mouse_tuning::Handle>,
    pub(crate) display: Option<display_control::Handle>,
    pub(crate) clipboard: Option<clipboard_history::Handle>,
    pub(crate) shelf: Option<notch_shelf::Handle>,
    pub(crate) color: Option<color_picker::Handle>,
    pub(crate) ocr: Option<text_extractor::Handle>,
    pub(crate) switcher: Option<window_switcher::Handle>,
    pub(crate) scaling: Option<display_scaling::Handle>,
    pub(crate) audio: Option<audio_control::Handle>,
    pub(crate) dock: Option<dock_previews::Handle>,
    pub(crate) expander: Option<text_expander::Handle>,
    /// Shared so that listing / moving items (which wait for the main
    /// thread) can run without holding `RUNNING`.
    pub(crate) menubar: Option<Arc<menubar_manager::Handle>>,
    /// The Workspaces editor shortcut (a `pm_hook` subscription).
    pub(crate) workspaces_hotkey: Option<pm_hook::Guard>,
}

impl Running {
    /// Whether any module that does something by itself is running.
    fn any(&self) -> bool {
        self.mwb.is_some()
            || self.zones.is_some()
            || self.scroll.is_some()
            || self.mouse.is_some()
            || self.keyboard.is_some()
            || self.mouse_tuning.is_some()
            || self.display.is_some()
            || self.clipboard.is_some()
            || self.shelf.is_some()
            || self.color.is_some()
            || self.ocr.is_some()
            || self.switcher.is_some()
            || self.scaling.is_some()
            || self.audio.is_some()
            || self.dock.is_some()
            || self.expander.is_some()
            || self.menubar.is_some()
    }
}

/// The config file in use (for the tray's "copy key" / "open config folder").
pub(crate) static CONFIG_PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Change the config file in use; the config watcher then applies it.
pub(crate) fn edit_config(f: impl FnOnce(&mut AppConfig)) -> Result<()> {
    let path = CONFIG_PATH.get().cloned().or_else(AppConfig::default_path).context("no config path")?;
    pm_config::edit_at(&path, f)
}

pub(crate) static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// The config file as it is on disk now (defaults if it can't be read).
pub(crate) fn current_config() -> AppConfig {
    CONFIG_PATH.get().and_then(|p| AppConfig::load_or_default(p).ok()).unwrap_or_default()
}

pub(crate) fn shutdown() {
    if let Some(mut r) = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take() {
        if let Some(m) = &r.mwb {
            m.stop();
            mwb_glue::write_stopped();
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
            k.stop(); // also puts the Caps Lock (hidutil) mapping back
        }
        if let Some(t) = r.expander.take() {
            t.stop();
        }
        if let Some(mut m) = r.mouse_tuning.take() {
            m.stop(); // restores the pointer settings
        }
        if let Some(d) = r.display.take() {
            d.stop(); // restores gamma tables
            display_glue::write_stopped();
        }
        if let Some(c) = r.clipboard.take() {
            c.stop();
        }
        if let Some(s) = r.shelf.take() {
            s.stop();
        }
        if let Some(w) = r.switcher.take() {
            w.stop();
        }
        if let Some(d) = r.dock.take() {
            d.stop();
        }
        if let Some(m) = r.menubar.take() {
            m.stop(); // short separators again: every icon visible
            menubar_glue::write_stopped();
        }
        if let Some(s) = r.scaling.take() {
            s.stop(); // removes the virtual HiDPI displays, restores modes
            scaling_glue::write_stopped();
        }
        if let Some(a) = r.audio.take() {
            a.stop(); // removes the per-app volume taps
            audio_glue::write_stopped();
        }
        drop(r.color.take());
        drop(r.ocr.take());
        drop(r.workspaces_hotkey.take());
    }
}

/// Where the app logs when it isn't attached to a terminal (e.g. launched as
/// PowerMac.app): `~/Library/Logs/PowerMac/powermac.log` on macOS,
/// `%LOCALAPPDATA%\PowerMac\powermac.log` on Windows.
pub(crate) fn log_path() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    return dirs::home_dir().map(|h| h.join("Library/Logs/PowerMac/powermac.log"));
    #[cfg(not(target_os = "macos"))]
    return dirs::data_local_dir().map(|d| d.join("PowerMac").join("powermac.log"));
}

fn init_logging() {
    use std::io::IsTerminal;
    let mut b = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if !std::io::stderr().is_terminal() {
        if let Some(path) = log_path() {
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            // Keep one previous log around instead of growing forever.
            if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5_000_000) {
                let _ = std::fs::rename(&path, path.with_extension("log.old"));
            }
            if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                b.target(env_logger::Target::Pipe(Box::new(f)));
            }
        }
    }
    b.init();
}

fn main() -> Result<()> {
    init_logging();
    let cli = Cli::parse();
    let path = match cli.config {
        Some(p) => p,
        None => AppConfig::default_path().context("no config directory on this platform")?,
    };
    match cli.cmd.unwrap_or(Cmd::Run { no_tray: false, open_settings: None, wait_pid: None }) {
        Cmd::Run { no_tray, open_settings, wait_pid } => {
            if let Some(pid) = wait_pid {
                app_glue::wait_for_exit(pid);
            }
            app_glue::NO_TRAY.store(no_tray, std::sync::atomic::Ordering::Relaxed);
            run(&path, !no_tray, open_settings)
        }
        Cmd::Workspace { cmd } => workspace_cmd(&path, cmd),
        Cmd::Net { cmd } => net_cli::run(&path, cmd),
        Cmd::Permissions { json } => permissions_cmd(&path, json),
        Cmd::Update { cmd: UpdateCmd::Check { pre } } => update_cmd(&path, pre),
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
        Cmd::Mwb { cmd: MwbCmd::Doctor } => mwb_glue::doctor(&path),
        Cmd::Settings { page } => {
            if let Ok(exe) = std::env::current_exe() {
                settings_ui::set_cli_exe(exe);
            }
            settings_ui::run_with_page(path, page)
        }
        Cmd::Clipboard => {
            let cfg = AppConfig::load_or_default(&path)?;
            clipboard_history::run_picker(clipboard_history::data_dir_for_config(&path), cfg.clipboard_history)
        }
        Cmd::ColorPicker { editor } => color_picker_cmd(&path, editor),
        Cmd::Ocr { file, clipboard } => ocr_cmd(&path, file, clipboard),
        Cmd::Rename { paths } => file_tools::rename_ui::run_rename_window_with(
            paths,
            Some(pm_config::file_tools_path_for(&path)),
            Some(path.with_file_name("rename-undo.json")),
        ),
        Cmd::New { dir } => file_tools::newplus_ui::run_new_window_with(dir, Some(pm_config::file_tools_path_for(&path))),
        Cmd::Resize { args } => {
            // (-h is --height here)
            if args.is_empty() || args.iter().any(|a| a == "--help") {
                println!("{}", file_tools::resize::cli::USAGE);
                return Ok(());
            }
            let code = file_tools::resize::cli::run_with(&args, Some(&pm_config::file_tools_path_for(&path)))?;
            std::process::exit(code);
        }
        Cmd::Finder { args } => {
            if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
                println!("{}", file_tools::finder::USAGE);
                return Ok(());
            }
            let cfg = file_tools::Config::load_from(&pm_config::file_tools_path_for(&path));
            let out = file_tools::finder::run_cli_with(&args, &cfg.finder)?;
            if !out.is_empty() {
                println!("{out}");
            }
            Ok(())
        }
        Cmd::Editor { monitor } => settings_ui::run_editor(path, monitor),
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

/// `powermac color-picker [--editor]`
fn color_picker_cmd(path: &std::path::Path, editor: bool) -> Result<()> {
    let cfg = AppConfig::load_or_default(path)?.color_picker.settings;
    let dir = pm_config::data_dir_for(path);
    if editor {
        return color_picker::run_editor(dir, cfg);
    }
    if !color_picker::SUPPORTED {
        bail!("picking colors isn't supported on this platform");
    }
    let Some(c) = color_picker::pick_once() else {
        eprintln!("cancelled");
        return Ok(());
    };
    let text = cfg.copied_text(c);
    if let Err(e) = color_picker::history::add(&dir, c, cfg.history_limit) {
        log::warn!("color picker: saving history: {e:#}");
    }
    color_picker::copy_text(&text)?;
    println!("{text}");
    Ok(())
}

/// `powermac ocr [FILE] [--clipboard]`
fn ocr_cmd(path: &std::path::Path, file: Option<std::path::PathBuf>, clipboard: bool) -> Result<()> {
    let cfg = AppConfig::load_or_default(path)?.text_extractor.settings;
    let text = match (file, clipboard) {
        (Some(f), _) => text_extractor::ocr_image_with(&f, &cfg)?,
        (None, true) => text_extractor::ocr_clipboard(&cfg)?,
        (None, false) => {
            if text_extractor::screen_recording_permission() != text_extractor::Permission::Granted {
                eprintln!("note: capturing the screen needs Screen Recording permission");
            }
            match text_extractor::capture_once(&cfg)? {
                Some(t) => {
                    text_extractor::copy_text(&t)?;
                    t
                }
                None => {
                    eprintln!("cancelled");
                    return Ok(());
                }
            }
        }
    };
    println!("{text}");
    Ok(())
}

fn install_ctrlc() {
    let _ = ctrlc::set_handler(|| {
        log::info!("shutting down");
        shutdown();
        std::process::exit(0);
    });
}

/// `powermac workspace …`
fn workspace_cmd(path: &std::path::Path, cmd: WorkspaceCmd) -> Result<()> {
    let cfg = AppConfig::load_or_default(path)?.workspaces;
    let dir = pm_config::data_dir_for(path);
    match cmd {
        WorkspaceCmd::List => {
            let list = workspaces::list(&dir)?;
            if list.is_empty() {
                println!("no workspaces yet (capture one with `powermac workspace capture`)");
            }
            let now = workspaces::model::now_unix();
            for w in list {
                let last = w.last_launched_time.map(|t| workspaces::model::relative_time(t, now)).unwrap_or_else(|| "never".into());
                println!("{}  {}  ({} windows, launched {last})", w.id, w.name, w.apps.len());
            }
            Ok(())
        }
        WorkspaceCmd::Capture { name } => {
            if !workspaces::has_permissions() {
                bail!("capturing windows needs Accessibility permission (System Settings → Privacy & Security → Accessibility)");
            }
            let ws = workspaces::capture_and_save(&dir, name.as_deref().unwrap_or(""), &cfg)?;
            println!("saved \"{}\" ({}) with {} windows", ws.name, ws.id, ws.apps.len());
            Ok(())
        }
        WorkspaceCmd::Launch { name } => {
            if !workspaces::has_permissions() {
                bail!("moving windows needs Accessibility permission (System Settings → Privacy & Security → Accessibility)");
            }
            let report = workspaces::launch(&dir, &name, &cfg, &|ev| {
                if ev.status.is_final() {
                    println!("  {}: {}{}", ev.app, ev.status.label(), ev.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default());
                }
            })?;
            println!("{} in {:.1} s", report.summary(), report.elapsed.as_secs_f32());
            if !report.all_ok() {
                std::process::exit(1);
            }
            Ok(())
        }
        WorkspaceCmd::Edit => workspaces::run_editor_with(dir, cfg),
    }
}

/// `powermac permissions [--json]`
fn permissions_cmd(path: &std::path::Path, json: bool) -> Result<()> {
    use settings_ui::permissions as perms;
    let probe = perms::probe_in_process();
    if json {
        println!("{}", serde_json::to_string(&probe)?);
        return Ok(());
    }
    let cfg = AppConfig::load_or_default(path)?;
    for n in perms::needs(&cfg) {
        let g = probe.get(n.perm);
        let used = if n.features.is_empty() { "not used by the enabled features".to_string() } else { n.features.join(", ") };
        println!("{:<32} {:<14} {used}", n.perm.title(), format!("{g:?}"));
        if !g.granted() && cfg!(target_os = "macos") {
            println!("{:<32} open {}", "", n.perm.settings_url());
        }
    }
    Ok(())
}

/// `powermac update check [--pre]`
fn update_cmd(path: &std::path::Path, pre: bool) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let r = pm_update::check(&pm_update::CheckOptions::new(version, pre));
    let mut st: pm_update::UpdateState = pm_config::bridge::read(path, pm_config::bridge::UPDATE_STATUS_FILE).map(|s| s.data).unwrap_or_default();
    let failed = r.as_ref().err().map(|e| e.to_string());
    st.record(version, r);
    if path.parent().is_some_and(|d| d.exists()) {
        let _ = pm_config::bridge::write(path, pm_config::bridge::UPDATE_STATUS_FILE, &st);
    }
    println!("{}", st.summary(version));
    if let Some(r) = st.available(version) {
        println!("{} — {}", r.name, r.best_link());
    }
    if let Some(e) = failed {
        bail!("{e}");
    }
    Ok(())
}

fn run(path: &std::path::Path, tray: bool, open_settings: Option<String>) -> Result<()> {
    // First launch (e.g. opening PowerMac.app): create a config with a fresh key.
    if !path.exists() {
        AppConfig::with_new_key().save(path)?;
        log::info!("created {} with a new Mouse Without Borders key", path.display());
    }
    let cfg = AppConfig::load_or_default(path)?;
    let _ = CONFIG_PATH.set(path.to_path_buf());
    // Display Control's OSD, the notch shelf and the Carbon hotkeys (Color
    // Picker, Text Extractor) need the shared NSApplication before they start.
    #[cfg(target_os = "macos")]
    {
        let mtm = objc2::MainThreadMarker::new().context("run must be on the main thread")?;
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    }
    let mut running = Running::default();
    modules::apply(&mut running, None, &cfg);
    modules::watch(path, cfg.clone());
    app_glue::start_once();

    let anything = running.any() || cfg.awake.enabled;
    if !anything && !tray {
        bail!("no module is running (check the config and permissions with `powermac doctor`)");
    }
    *RUNNING.lock().unwrap() = Some(running);
    install_ctrlc();

    #[cfg(target_os = "macos")]
    {
        if tray {
            // NSStatusItem needs the shared NSApplication (created above).
            tray::install()?;
            open_setup_if_needed(&cfg, open_settings.as_deref());
        }
        // The FancyZones overlay and the tray are AppKit UI: run the app loop here.
        fancyzones::platform::run_main_loop();
    }

    #[cfg(windows)]
    {
        if tray {
            tray::install()?;
            open_setup_if_needed(&cfg, open_settings.as_deref());
        }
        win_message_loop();
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (tray, open_settings);
        let (_tx, rx) = std::sync::mpsc::channel::<()>();
        let _ = rx.recv();
    }
    #[allow(unreachable_code)]
    {
        shutdown();
        Ok(())
    }
}

/// Open the settings window at startup: on `page` after a restart, else the
/// setup on first launch or when a needed permission is missing.
#[cfg(any(target_os = "macos", windows))]
fn open_setup_if_needed(cfg: &AppConfig, page: Option<&str>) {
    let page = page.or_else(|| app_glue::startup_page(cfg));
    if let Some(p) = page {
        log::info!("opening settings on {p:?}");
        children::open(&children::SETTINGS, &["settings", "--page", p]);
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
    println!("modules: mwb={} fancyzones={} awake={} scroll={} mouse-tuning={} keyboard={} displays={} \
              clipboard={} shelf={} color-picker={} text-extractor={} window-switcher={} display-scaling={} \
              sound={} dock-previews={} text-expander={} menu-bar={}",
        cfg.mwb.enabled, cfg.fancyzones.enabled, cfg.awake.enabled, cfg.scroll_reverser.enabled,
        cfg.mouse_tuning.enabled, cfg.keyboard_manager.enabled, cfg.display_control.enabled,
        cfg.clipboard_history.enabled, cfg.notch_shelf.enabled, cfg.color_picker.enabled,
        cfg.text_extractor.enabled, cfg.window_switcher.enabled, cfg.display_scaling.enabled,
        cfg.audio.enabled, cfg.dock_previews.enabled, cfg.text_expander.enabled, cfg.menu_bar.enabled);
    println!("caps lock: {:?}", cfg.keyboard_manager.hyper_key.mode);
    println!("machine name: {}", cfg.mwb.settings.effective_machine_name());
    println!("mwb key valid: {}", mwb::crypto::is_key_valid(&cfg.mwb.settings.key));
    let backend: Arc<dyn pm_input::InputBackend> = pm_input::backend()?;
    println!("input permissions: {}", backend.has_permissions(prompt));
    println!("screen recording: {:?}", text_extractor::screen_recording_permission());
    println!("finder quick actions installed: {}{}", file_tools::finder_integration_installed(),
        if file_tools::finder_integration_needs_update() { " (some are missing: reinstall them in Settings → File Tools)" } else { "" });
    println!("hidpi (virtual displays) supported: {}", display_scaling::hidpi_supported());
    println!("per-app volume supported: {} (audio capture permission: {:?})",
        audio_control::per_app_volume_supported(), audio_control::capture_permission());
    for (i, s) in backend.screens()?.iter().enumerate() {
        println!("screen {i}: {}x{} at ({}, {})", s.width(), s.height(), s.left, s.top);
    }
    Ok(())
}
