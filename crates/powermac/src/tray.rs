//! Menu-bar (macOS) / notification-area (Windows) icon.
//!
//! Menu items are not `Send`, so they live in a thread-local on the main
//! thread. Menu clicks already arrive on the main thread; the periodic status
//! refresh is marshalled there (main dispatch queue on macOS, a thread timer
//! in the message loop on Windows).

use std::cell::RefCell;
use std::time::Duration;

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::{shutdown, Running, RUNNING};

const AWAKE_CHOICES: &[(&str, Option<u64>)] = &[
    ("Indefinitely", None),
    ("30 minutes", Some(30 * 60)),
    ("1 hour", Some(60 * 60)),
    ("2 hours", Some(2 * 60 * 60)),
];

const LAYOUTS: &[(&str, fancyzones::LayoutType, i32)] = &[
    ("Focus", fancyzones::LayoutType::Focus, 3),
    ("Columns", fancyzones::LayoutType::Columns, 3),
    ("Rows", fancyzones::LayoutType::Rows, 3),
    ("Grid", fancyzones::LayoutType::Grid, 4),
    ("Priority grid", fancyzones::LayoutType::PriorityGrid, 3),
];

struct Ui {
    tray: TrayIcon,
    menu: Menu,
    settings: MenuItem,
    /// "Setup & permissions…"
    setup: MenuItem,
    /// "Needs Accessibility…" (only in the menu while something is missing).
    alert: MenuItem,
    alert_shown: Option<String>,
    alert_checked_at: Option<std::time::Instant>,
    /// "Update available: vX.Y.Z" (only while one is).
    update: MenuItem,
    update_shown: Option<String>,
    /// The icon has the warning dot.
    badged: bool,
    workspaces: Workspaces,
    mwb_status: MenuItem,
    mwb_find: MenuItem,
    mwb_switch: Submenu,
    mwb_machines: Vec<String>,
    awake_status: MenuItem,
    awake_off: CheckMenuItem,
    awake_choices: Vec<CheckMenuItem>,
    awake_display: CheckMenuItem,
    zones_status: MenuItem,
    layouts: Vec<CheckMenuItem>,
    edit_layouts: MenuItem,
    copy_key: MenuItem,
    login: CheckMenuItem,
    find_mouse: MenuItem,
    paste_plain: MenuItem,
    highlighter: CheckMenuItem,
    crosshairs: CheckMenuItem,
    scroll_mouse: CheckMenuItem,
    scroll_trackpad: CheckMenuItem,
    open_privacy: MenuItem,
    open_config: MenuItem,
    open_log: MenuItem,
    quit: MenuItem,
    tools: Tools,
    sound: Submenu,
    /// What the Sound submenu shows (to rebuild only on change).
    sound_shown: Option<SoundSig>,
    battery: Submenu,
    battery_status: MenuItem,
    battery_detail: MenuItem,
    battery_full: MenuItem,
    battery_settings: MenuItem,
    /// "Network activity…" (the settings window's Network page).
    network: MenuItem,
}

/// Sound submenu contents: devices (kind, uid, name, default) and, with
/// per-app volume on, apps (bundle id, name, volume %, muted).
type SoundSig = Option<(Vec<(char, String, String, bool)>, Option<Vec<(String, String, u32, bool)>>)>;

/// Per-app volume presets in the Sound submenu (percent).
const APP_PRESETS: [u32; 5] = [25, 50, 75, 100, 150];

/// The Workspaces submenu.
struct Workspaces {
    menu: Submenu,
    capture: MenuItem,
    edit: MenuItem,
    /// (workspaces.json stamp, module on, count) of what's shown.
    shown: Option<(Option<std::time::SystemTime>, bool, usize)>,
    checked_at: Option<std::time::Instant>,
}

/// The "Tools" part of the menu.
struct Tools {
    pick_color: MenuItem,
    extract_text: MenuItem,
    clip: Submenu,
    clip_open: MenuItem,
    clip_pause: MenuItem,
    clip_clear: MenuItem,
    /// The recent items shown (id, label), to rebuild only on change.
    clip_shown: Option<Vec<(u64, String)>>,
    switcher: MenuItem,
    shelf: MenuItem,
    /// Opens the settings window on the Notch Widgets page.
    notch_widgets: MenuItem,
    displays: Submenu,
    /// (id, name, has volume) of the displays shown.
    displays_shown: Option<Vec<(String, String, bool)>>,
    display_items: Vec<DisplayItems>,
    finder: CheckMenuItem,
    eject: MenuItem,
    hidden_files: CheckMenuItem,
    menubar_show: MenuItem,
    hidden_checked_at: std::time::Instant,
    /// The hotkeys shown as accelerators (to update only on change).
    accels: [Option<String>; 4],
    finder_checked_at: std::time::Instant,
}

/// One display's items: (id, brightness presets, volume presets, mute).
type DisplayItems = (String, Vec<CheckMenuItem>, Vec<CheckMenuItem>, Option<CheckMenuItem>);

/// Clipboard history state for the menu: (paused, hotkey, recent items).
type ClipState = (bool, Option<keyboard_manager::Shortcut>, Vec<(u64, String)>);

/// Brightness / volume presets in the Displays submenu.
const DISPLAY_PRESETS: [u8; 5] = [0, 25, 50, 75, 100];
/// Clipboard items shown in the menu.
const CLIP_ITEMS: usize = 10;

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Create the tray icon. Must be called on the main thread, before the loop runs.
pub fn install() -> anyhow::Result<()> {
    let menu = Menu::new();
    let settings = MenuItem::new("Settings…", true, None);
    let setup = MenuItem::new("Setup & permissions…", true, None);
    let alert = MenuItem::new("Permissions missing", true, None);
    let update = MenuItem::new("Update available", true, None);
    let ws_menu = Submenu::new("Workspaces", true);
    let ws_capture = MenuItem::new("Capture desktop…", true, None);
    let ws_edit = MenuItem::new("Edit workspaces…", true, None);
    let title = MenuItem::new("PowerMac", false, None);

    let mwb_status = MenuItem::new("Mouse Without Borders: off", false, None);
    let mwb_find = MenuItem::new("Find computers now", false, None);
    let mwb_switch = Submenu::new("Switch to", false);

    let awake = Submenu::new("Awake", true);
    let awake_status = MenuItem::new("Not keeping awake", false, None);
    let awake_off = CheckMenuItem::new("Off", true, true, None);
    let awake_choices: Vec<_> =
        AWAKE_CHOICES.iter().map(|(t, _)| CheckMenuItem::new(*t, true, false, None)).collect();
    let awake_display = CheckMenuItem::new("Keep screen on", true, false, None);
    awake.append_items(&[&awake_status, &PredefinedMenuItem::separator(), &awake_off])?;
    for c in &awake_choices {
        awake.append(c)?;
    }
    awake.append_items(&[&PredefinedMenuItem::separator(), &awake_display])?;

    let zones = Submenu::new("FancyZones", true);
    let zones_status = MenuItem::new("FancyZones: off", false, None);
    let layouts: Vec<_> = LAYOUTS.iter().map(|(t, _, _)| CheckMenuItem::new(*t, false, false, None)).collect();
    zones.append_items(&[&zones_status, &PredefinedMenuItem::separator()])?;
    for l in &layouts {
        zones.append(l)?;
    }
    let edit_layouts = MenuItem::new("Edit layouts…", true, None);
    zones.append_items(&[&PredefinedMenuItem::separator(), &edit_layouts])?;

    let copy_key = MenuItem::new("Copy security key", true, None);
    let open_privacy = MenuItem::new("Open Privacy settings…", cfg!(target_os = "macos"), None);
    let scroll = Submenu::new("Scroll direction", true);
    let scroll_mouse = CheckMenuItem::new("Reverse mouse wheel", true, false, None);
    let scroll_trackpad = CheckMenuItem::new("Reverse trackpad", true, false, None);
    scroll.append_items(&[&scroll_mouse, &scroll_trackpad])?;
    let mouse = Submenu::new("Mouse utilities", true);
    let find_mouse = MenuItem::new("Find my mouse", true, None);
    let highlighter = CheckMenuItem::new("Highlight clicks", true, false, None);
    let crosshairs = CheckMenuItem::new("Crosshairs", true, false, None);
    mouse.append_items(&[&find_mouse, &highlighter, &crosshairs])?;
    let paste_plain = MenuItem::new("Paste as plain text", true, None);
    let open_config = MenuItem::new("Open config folder", true, None);
    let open_log = MenuItem::new("Open log file", true, None);
    let login = CheckMenuItem::new("Launch at login", true, pm_config::login::is_enabled(), None);
    let quit = MenuItem::new("Quit PowerMac", true, None);

    // Tools
    let tools_title = MenuItem::new("Tools", false, None);
    let pick_color = MenuItem::new("Pick color", false, None);
    let extract_text = MenuItem::new("Extract text", false, None);
    let clip = Submenu::new("Clipboard history", false);
    let clip_open = MenuItem::new("Open clipboard history…", true, None);
    let clip_pause = MenuItem::new("Pause recording", true, None);
    let clip_clear = MenuItem::new("Clear history", true, None);
    let switcher = MenuItem::new("Window switcher", false, None);
    let shelf = MenuItem::new("Show shelf", false, None);
    let notch_widgets = MenuItem::new("Notch widgets…", cfg!(target_os = "macos"), None);
    let displays = Submenu::new("Displays", false);
    let finder = CheckMenuItem::new(
        if cfg!(windows) { "Explorer menu: PowerRename, New+, …" } else { "Finder Quick Actions: PowerRename, New+, …" },
        cfg!(any(target_os = "macos", windows)),
        file_tools::finder_integration_installed(),
        None,
    );
    let menubar_show = MenuItem::new("Show hidden menu bar items", false, None);
    let eject = MenuItem::new("Eject all drives", cfg!(any(target_os = "macos", windows)), None);
    let hidden_files = CheckMenuItem::new(
        if cfg!(windows) { "Show hidden files (restarts Explorer windows)" } else { "Show hidden files (restarts Finder)" },
        cfg!(any(target_os = "macos", windows)),
        file_tools::hidden_files_shown(),
        None,
    );

    // Sound and Battery (filled in by the refresh).
    let sound = Submenu::new("Sound", false);
    let battery = Submenu::new("Battery", false);
    let battery_status = MenuItem::new("", false, None);
    let battery_detail = MenuItem::new("", false, None);
    let battery_full = MenuItem::new("Charge to 100 % now", false, None);
    let battery_settings = MenuItem::new("Battery settings…", true, None);
    battery.append_items(&[
        &battery_status,
        &battery_detail,
        &PredefinedMenuItem::separator(),
        &battery_full,
        &battery_settings,
    ])?;
    let network = MenuItem::new("Network activity…", true, None);

    menu.append_items(&[
        &settings,
        &setup,
        &PredefinedMenuItem::separator(),
        &title,
        &PredefinedMenuItem::separator(),
        &mwb_status,
        &mwb_find,
        &mwb_switch,
        &copy_key,
        &PredefinedMenuItem::separator(),
        &awake,
        &sound,
        &battery,
        &zones,
        &ws_menu,
        &scroll,
        &mouse,
        &paste_plain,
        &PredefinedMenuItem::separator(),
        &tools_title,
        &pick_color,
        &extract_text,
        &clip,
        &switcher,
        &shelf,
        &notch_widgets,
        &displays,
        &menubar_show,
        &finder,
        &eject,
        &hidden_files,
        &network,
        &PredefinedMenuItem::separator(),
        &login,
        &open_privacy,
        &open_config,
        &open_log,
        &quit,
    ])?;

    let builder = TrayIconBuilder::new().with_menu(Box::new(menu.clone())).with_tooltip("PowerMac");
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(icon());
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(icon());
    let tray = builder.build()?;

    UI.with(|u| {
        *u.borrow_mut() = Some(Ui {
            tray,
            menu,
            settings,
            setup,
            alert,
            alert_shown: None,
            alert_checked_at: None,
            update,
            update_shown: None,
            badged: false,
            workspaces: Workspaces { menu: ws_menu, capture: ws_capture, edit: ws_edit, shown: None, checked_at: None },
            mwb_status,
            mwb_find,
            mwb_switch,
            mwb_machines: Vec::new(),
            awake_status,
            awake_off,
            awake_choices,
            awake_display,
            zones_status,
            layouts,
            edit_layouts,
            copy_key,
            login,
            scroll_mouse,
            scroll_trackpad,
            find_mouse,
            paste_plain,
            highlighter,
            crosshairs,
            open_privacy,
            open_config,
            open_log,
            quit,
            tools: Tools {
                pick_color,
                extract_text,
                clip,
                clip_open,
                clip_pause,
                clip_clear,
                clip_shown: None,
                switcher,
                shelf,
                notch_widgets,
                displays,
                displays_shown: None,
                display_items: Vec::new(),
                finder,
                eject,
                hidden_files,
                menubar_show,
                hidden_checked_at: std::time::Instant::now(),
                accels: Default::default(),
                finder_checked_at: std::time::Instant::now(),
            },
            sound,
            sound_shown: None,
            battery,
            battery_status,
            battery_detail,
            battery_full,
            battery_settings,
            network,
        })
    });

    MenuEvent::set_event_handler(Some(|e: MenuEvent| {
        UI.with(|u| {
            if let Some(ui) = u.borrow_mut().as_mut() {
                on_click(ui, &e.id);
            }
        })
    }));
    refresh();
    start_refresh_timer();
    Ok(())
}

fn with_running<R>(f: impl FnOnce(&mut Running) -> R) -> Option<R> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(f)
}

/// Like [`with_running`] but gives up instead of waiting when the modules
/// are being reconfigured (the config watcher holds the lock and may be
/// waiting for this main thread, e.g. to register a Carbon hotkey).
fn try_with_running<R>(f: impl FnOnce(&mut Running) -> R) -> Option<R> {
    match RUNNING.try_lock() {
        Ok(mut g) => g.as_mut().map(f),
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner().as_mut().map(f),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

fn on_click(ui: &mut Ui, id: &tray_icon::menu::MenuId) {
    if id == ui.quit.id() {
        shutdown();
        std::process::exit(0);
    }
    if on_tools_click(&mut ui.tools, id) {
        return;
    }
    if let Some(rest) = id.as_ref().strip_prefix("snd-") {
        sound_click(rest);
        ui.sound_shown = None;
        return;
    }
    if id == ui.battery_full.id() {
        std::thread::spawn(|| {
            with_running(|r| {
                if let Some(b) = &r.battery {
                    log::info!("battery: charging to 100 % until unplugged (tray)");
                    b.charge_to_full_once();
                }
            });
        });
        ui.battery_full.set_enabled(false);
        return;
    }
    if id == ui.settings.id() {
        open_settings();
        return;
    }
    if id == ui.battery_settings.id() {
        open_settings_page("battery");
        return;
    }
    if id == ui.network.id() {
        open_settings_page("network");
        return;
    }
    if id == ui.setup.id() {
        open_settings_page("welcome");
        return;
    }
    if id == ui.alert.id() {
        open_settings_page("permissions");
        return;
    }
    if id == ui.update.id() {
        open_settings_page("general");
        return;
    }
    if id == ui.workspaces.capture.id() {
        crate::app_glue::capture_workspace(String::new());
        return;
    }
    if id == ui.workspaces.edit.id() {
        crate::app_glue::open_workspaces_editor();
        return;
    }
    if let Some(ws) = id.as_ref().strip_prefix("ws-launch:") {
        crate::app_glue::launch_workspace(ws.to_string());
        return;
    }
    if id == ui.edit_layouts.id() {
        open_editor();
        return;
    }
    if id == ui.paste_plain.id() {
        // Paste into the app that was frontmost before the menu opened.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(250));
            with_running(|r| r.keyboard.as_ref().map(|k| k.paste_plain_text()));
        });
        return;
    }
    if id == ui.mwb_find.id() {
        with_running(|r| r.mwb.as_ref().map(|m| m.discover_now()));
        ui.mwb_find.set_text("Searching…");
        ui.mwb_find.set_enabled(false);
        return;
    }
    if id == ui.find_mouse.id() {
        with_running(|r| r.mouse.as_ref().map(|m| m.find_mouse()));
        return;
    }
    if id == ui.highlighter.id() {
        with_running(|r| r.mouse.as_ref().map(|m| m.toggle_highlighter()));
    } else if id == ui.crosshairs.id() {
        with_running(|r| r.mouse.as_ref().map(|m| m.toggle_crosshairs()));
    }
    if id == ui.scroll_mouse.id() || id == ui.scroll_trackpad.id() {
        let (mouse, trackpad) = (ui.scroll_mouse.is_checked(), ui.scroll_trackpad.is_checked());
        let r = crate::edit_config(|c| {
            let s = &mut c.scroll_reverser;
            s.reverse_mouse_vertical = mouse;
            s.reverse_trackpad_vertical = trackpad;
            // `enabled` covers smooth scrolling too.
            s.enabled = mouse
                || trackpad
                || s.reverse_mouse_horizontal
                || s.reverse_trackpad_horizontal
                || s.smooth_scrolling;
        });
        if let Err(e) = r {
            log::warn!("scroll reverser: {e:#}");
        }
        return;
    }
    if id == ui.login.id() {
        match pm_config::login::set_enabled(ui.login.is_checked()) {
            Ok(()) => log::info!("launch at login: {}", if ui.login.is_checked() { "on" } else { "off" }),
            Err(e) => log::warn!("launch at login: {e:#}"),
        }
        ui.login.set_checked(pm_config::login::is_enabled());
        return;
    }
    if id == ui.copy_key.id() {
        copy_key();
        return;
    }
    if id == ui.open_privacy.id() {
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn();
        return;
    }
    if id == ui.open_log.id() {
        if let Some(p) = crate::log_path().filter(|p| p.exists()) {
            #[cfg(target_os = "macos")]
            let _ = std::process::Command::new("open").arg("-R").arg(&p).spawn();
            #[cfg(windows)]
            let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
        }
        return;
    }
    if id == ui.open_config.id() {
        open_config_dir();
        return;
    }
    if id == ui.awake_off.id() {
        with_running(|r| r.awake.as_ref().map(|a| a.stop()));
    } else if let Some(i) = ui.awake_choices.iter().position(|c| c.id() == id) {
        set_awake(AWAKE_CHOICES[i].1, ui.awake_display.is_checked());
    } else if id == ui.awake_display.id() {
        // Re-apply the current mode with the new screen setting.
        let display = ui.awake_display.is_checked();
        let st = with_running(|r| r.awake.as_ref().map(|a| a.status())).flatten();
        if let Some(st) = st.filter(|s| s.active) {
            let mode = match st.remaining {
                Some(left) => awake::AwakeMode::Timed(left),
                None => st.mode,
            };
            with_running(|r| {
                r.awake.as_ref().map(|a| a.set(awake::AwakeConfig { mode, keep_display_on: display, bound_pid: st.bound_pid }))
            });
        }
    } else if let Some(i) = ui.layouts.iter().position(|c| c.id() == id) {
        let (_, kind, zones) = LAYOUTS[i];
        with_running(|r| {
            if let Some(z) = &r.zones {
                for m in z.monitors().unwrap_or_default() {
                    if let Err(e) = z.apply_layout(&m.id, fancyzones::AppliedLayout::template(kind, zones)) {
                        log::warn!("fancyzones: {e:#}");
                    }
                }
            }
        });
    } else if let Some(name) = id.as_ref().strip_prefix("mwb-switch:") {
        let name = name.to_string();
        with_running(|r| r.mwb.as_ref().map(|m| m.switch_to(&name)));
    }
    refresh_ui(ui);
}

fn set_awake(secs: Option<u64>, display: bool) {
    let mode = match secs {
        Some(s) => awake::AwakeMode::Timed(Duration::from_secs(s)),
        None => awake::AwakeMode::Indefinite,
    };
    with_running(|r| {
        if let Some(a) = &r.awake {
            if let Err(e) = a.set(awake::AwakeConfig { mode, keep_display_on: display, bound_pid: None }) {
                log::warn!("awake: {e}");
            }
        }
    });
}

fn refresh() {
    UI.with(|u| {
        if let Some(ui) = u.borrow_mut().as_mut() {
            refresh_ui(ui);
        }
    })
}

fn refresh_ui(ui: &mut Ui) {
    refresh_tools(&mut ui.tools);
    refresh_sound(ui);
    refresh_battery(ui);
    refresh_alerts(ui);
    refresh_workspaces(&mut ui.workspaces);
    let snapshot = try_with_running(|r| {
        (
            r.mwb.as_ref().map(|m| m.status()),
            r.awake.as_ref().map(|a| a.status()),
            r.zones.as_ref().map(|z| z.config()),
            r.zones.as_ref().and_then(|z| z.monitors().ok()),
            r.scroll.as_ref().map(|s| s.config()),
            r.mouse.as_ref().map(|m| (m.highlighter_on(), m.crosshairs_on())),
        )
    });
    let Some((mwb, awake, zones, monitors, scroll, mouse)) = snapshot else { return };

    // The settings window can change this too (only cheap to check on macOS).
    #[cfg(target_os = "macos")]
    ui.login.set_checked(pm_config::login::is_enabled());

    // Mouse utilities
    let (hl, ch) = mouse.unwrap_or((false, false));
    ui.find_mouse.set_enabled(mouse.is_some());
    ui.highlighter.set_enabled(mouse.is_some());
    ui.crosshairs.set_enabled(mouse.is_some());
    ui.highlighter.set_checked(hl);
    ui.crosshairs.set_checked(ch);

    // Scroll reverser
    let sc = scroll.unwrap_or(scroll_reverser::Config { enabled: false, ..Default::default() });
    ui.scroll_mouse.set_checked(sc.enabled && sc.reverse_mouse_vertical);
    ui.scroll_trackpad.set_checked(sc.enabled && sc.reverse_trackpad_vertical);

    // Mouse Without Borders
    match &mwb {
        None => {
            ui.mwb_status.set_text("Mouse Without Borders: off");
            ui.mwb_find.set_text("Find computers now");
            ui.mwb_find.set_enabled(false);
            ui.mwb_switch.set_enabled(false);
        }
        Some(st) => {
            ui.mwb_status.set_text(crate::mwb_glue::status_line(st));
            ui.mwb_find.set_text(if st.searching { "Searching…" } else { "Find computers now" });
            ui.mwb_find.set_enabled(!st.searching);
            let machines: Vec<String> = st.matrix.names().map(str::to_string).collect();
            if machines != ui.mwb_machines {
                for item in ui.mwb_switch.items() {
                    if let Some(i) = item.as_menuitem() {
                        let _ = ui.mwb_switch.remove(i);
                    }
                }
                for n in &machines {
                    let _ = ui.mwb_switch.append(&MenuItem::with_id(format!("mwb-switch:{n}"), n, true, None));
                }
                ui.mwb_machines = machines;
            }
            ui.mwb_switch.set_enabled(!ui.mwb_machines.is_empty());
        }
    }

    // Awake
    if let Some(st) = &awake {
        let active = st.active;
        ui.awake_off.set_checked(!active);
        let secs = if active { st.remaining.map(|d| d.as_secs()) } else { None };
        for (i, c) in ui.awake_choices.iter().enumerate() {
            let matches = active
                && match (AWAKE_CHOICES[i].1, st.mode) {
                    (None, awake::AwakeMode::Indefinite) => true,
                    (Some(s), awake::AwakeMode::Timed(d)) => d.as_secs() == s,
                    _ => false,
                };
            c.set_checked(matches);
        }
        if active {
            ui.awake_display.set_checked(st.keep_display_on);
        }
        ui.awake_status.set_text(match (active, secs) {
            (false, _) => "Not keeping awake".to_string(),
            (true, None) => "Awake indefinitely".to_string(),
            (true, Some(s)) => format!("Awake: {}h {:02}m left", s / 3600, (s % 3600) / 60),
        });
    }

    // FancyZones
    match (&zones, &monitors) {
        (Some(cfg), Some(mons)) => {
            ui.zones_status.set_text("FancyZones: running (Shift+drag)");
            let current = mons.first().and_then(|m| cfg.applied_layouts.get(&m.id)).map(|l| l.kind);
            for (i, c) in ui.layouts.iter().enumerate() {
                c.set_enabled(true);
                c.set_checked(current == Some(LAYOUTS[i].1));
            }
        }
        _ => {
            ui.zones_status.set_text("FancyZones: off (needs Accessibility)");
            for c in &ui.layouts {
                c.set_enabled(false);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn start_refresh_timer() {
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(2));
        dispatch2::DispatchQueue::main().exec_async(refresh);
    });
}

#[cfg(not(target_os = "macos"))]
fn start_refresh_timer() {
    // Windows: the main message loop calls `refresh_tick` on its timer.
}

#[cfg(not(target_os = "macos"))]
pub fn refresh_tick() {
    refresh();
}

fn copy_key() {
    let Some(path) = crate::CONFIG_PATH.get().cloned().or_else(pm_config::AppConfig::default_path) else { return };
    let key = match pm_config::AppConfig::load_or_default(&path) {
        Ok(c) => c.mwb.settings.key,
        Err(e) => {
            log::warn!("{e:#}");
            return;
        }
    };
    match arboard::Clipboard::new().and_then(|mut c| c.set_text(key)) {
        Ok(()) => log::info!("security key copied to the clipboard"),
        Err(e) => log::warn!("clipboard: {e}"),
    }
}

/// Start `powermac --config <path> settings` as a separate process (its
/// event loop can't share this process' main loop), or bring the one that
/// is already open to the front.
fn open_settings() {
    crate::children::open(&crate::children::SETTINGS, &["settings"]);
}

/// Open the settings window on `page` ("notch-widgets"); an open window is
/// brought to the front and asked to switch pages.
fn open_settings_page(page: &str) {
    if crate::children::open_checked(&crate::children::SETTINGS, &["settings", "--page", page]) {
        if let Some(path) = crate::CONFIG_PATH.get().cloned().or_else(pm_config::AppConfig::default_path) {
            let req = pm_config::bridge::SettingsRequest::ShowPage { page: page.to_string() };
            if let Err(e) = pm_config::bridge::write(&path, pm_config::bridge::SETTINGS_REQUEST_FILE, &req) {
                log::warn!("settings: {e:#}");
            }
        }
    }
}

/// Start `powermac --config <path> editor` (the FancyZones layout editor),
/// or bring the one that is already open to the front.
fn open_editor() {
    crate::children::open(&crate::children::EDITOR, &["editor"]);
}

fn open_config_dir() {
    let Some(dir) = crate::CONFIG_PATH.get().cloned().or_else(pm_config::AppConfig::default_path).and_then(|p| p.parent().map(|d| d.to_path_buf())) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&dir).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
}

fn icon() -> Icon {
    icon_with(false)
}

/// 32x32 icon: a screen outline split into three zones. Black (template) on
/// macOS so it follows the menu bar's appearance; blue on Windows. With
/// `badge`, a dot in the top-right corner (a permission is missing).
fn icon_with(badge: bool) -> Icon {
    const N: usize = 32;
    let (r, g, b) = if cfg!(target_os = "macos") { (0, 0, 0) } else { (0x2b, 0x88, 0xd8) };
    let mut px = vec![0u8; N * N * 4];
    let mut set = |x: usize, y: usize| {
        let i = (y * N + x) * 4;
        px[i..i + 4].copy_from_slice(&[r, g, b, 255]);
    };
    let (l, t, rr, bb) = (2, 5, 29, 26);
    for x in l..=rr {
        for w in 0..2 {
            set(x, t + w);
            set(x, bb - w);
        }
    }
    for y in t..=bb {
        for w in 0..2 {
            set(l + w, y);
            set(rr - w, y);
        }
    }
    // Zone dividers: one vertical, and a horizontal on the right half.
    for y in t..=bb {
        set(14, y);
        set(15, y);
    }
    for x in 15..=rr {
        set(x, 15);
        set(x, 16);
    }
    // Filled "active" zone on the left.
    for y in t + 4..=bb - 4 {
        for x in l + 4..=11 {
            set(x, y);
        }
    }
    if badge {
        // Clear a ring, then the dot (orange on Windows; the menu bar color on macOS).
        let (cx, cy) = (26.0f32, 6.0f32);
        let dot = if cfg!(target_os = "macos") { [0, 0, 0, 255] } else { [0xE8, 0x7A, 0x00, 255] };
        for y in 0..N {
            for x in 0..N {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let i = (y * N + x) * 4;
                if d <= 5.5 {
                    px[i..i + 4].copy_from_slice(&dot);
                } else if d <= 7.5 {
                    px[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
    }
    Icon::from_rgba(px, N as u32, N as u32).expect("valid icon")
}

// ---------------------------------------------------------------------------
// Tools: Color Picker, Text Extractor, Clipboard History, Window Switcher,
// Notch Shelf, Displays, Finder Quick Actions

/// Handle a click on a Tools item; false if `id` isn't one.
fn on_tools_click(t: &mut Tools, id: &tray_icon::menu::MenuId) -> bool {
    if id == t.pick_color.id() {
        // Let the menu close before the loupe appears.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            with_running(|r| r.color.as_ref().map(|c| c.pick_now()));
        });
    } else if id == t.extract_text.id() {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            with_running(|r| r.ocr.as_ref().map(|o| o.capture_now()));
        });
    } else if id == t.clip_open.id() {
        with_running(|r| r.clipboard.as_ref().map(|c| c.open_picker()));
    } else if id == t.clip_pause.id() {
        let paused = with_running(|r| {
            r.clipboard.as_ref().map(|c| {
                let p = !c.paused();
                c.pause(p);
                p
            })
        })
        .flatten();
        if let Some(p) = paused {
            // Keep it across restarts (the watcher re-applies the same value).
            if let Err(e) = crate::edit_config(|c| c.clipboard_history.paused = p) {
                log::warn!("clipboard history: {e:#}");
            }
            log::info!("clipboard history: {}", if p { "paused" } else { "recording" });
        }
        t.clip_shown = None;
    } else if id == t.clip_clear.id() {
        if let Some(Err(e)) = with_running(|r| r.clipboard.as_ref().map(|c| c.clear())).flatten() {
            log::warn!("clipboard history: {e:#}");
        }
        t.clip_shown = None;
    } else if let Some(item) = id.as_ref().strip_prefix("clip:").and_then(|n| n.parse::<u64>().ok()) {
        // Paste into the app that was in front before the menu opened.
        if let Some(Err(e)) = with_running(|r| r.clipboard.as_ref().map(|c| c.paste_item(item))).flatten() {
            log::warn!("clipboard history: {e:#}");
        }
        t.clip_shown = None;
    } else if id == t.switcher.id() {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(150));
            with_running(|r| r.switcher.as_ref().map(|s| s.show()));
        });
    } else if id == t.shelf.id() {
        with_running(|r| r.shelf.as_ref().map(|s| s.expand()));
    } else if id == t.notch_widgets.id() {
        open_settings_page("notch-widgets");
    } else if id == t.menubar_show.id() {
        std::thread::spawn(|| {
            if let Some(h) = crate::menubar_glue::handle() {
                h.toggle();
            }
        });
    } else if id == t.eject.id() {
        eject_all();
    } else if id == t.hidden_files.id() {
        let show = t.hidden_files.is_checked();
        std::thread::spawn(move || match file_tools::finder::set_hidden_files_shown(show) {
            Ok(()) => log::info!("finder: hidden files {}", if show { "shown" } else { "hidden" }),
            Err(e) => log::warn!("finder: hidden files: {e:#}"),
        });
        t.hidden_checked_at = std::time::Instant::now();
    } else if id == t.finder.id() {
        toggle_finder_integration(t.finder.is_checked());
        t.finder.set_checked(file_tools::finder_integration_installed());
        t.finder_checked_at = std::time::Instant::now();
    } else if let Some(rest) = id.as_ref().strip_prefix("disp:") {
        display_click(rest);
        t.displays_shown = None; // re-read the values
    } else {
        return false;
    }
    true
}

/// `b:<pct>:<id>`, `v:<pct>:<id>` or `m:<id>`.
fn display_click(rest: &str) {
    let (kind, rest) = rest.split_once(':').unwrap_or(("", ""));
    let r = with_running(|r| {
        let d = r.display.as_ref()?;
        Some(match kind {
            "m" => {
                let muted = d.display(rest).and_then(|i| i.muted).unwrap_or(false);
                d.set_mute(rest, !muted)
            }
            _ => {
                let (pct, id) = rest.split_once(':')?;
                let pct: u8 = pct.parse().ok()?;
                if kind == "b" {
                    d.set_brightness(id, pct)
                } else {
                    d.set_volume(id, pct)
                }
            }
        })
    })
    .flatten();
    if let Some(Err(e)) = r {
        log::warn!("display control: {e:#}");
    }
}

fn toggle_finder_integration(install: bool) {
    if install {
        // The Quick Actions run this executable (inside PowerMac.app when
        // started from the bundle).
        match std::env::current_exe().map_err(anyhow::Error::from).and_then(|exe| file_tools::install_finder_integration(&exe)) {
            Ok(v) => log::info!("finder integration installed ({} items)", v.len()),
            Err(e) => log::warn!("finder integration: {e:#}"),
        }
    } else {
        match file_tools::uninstall_finder_integration() {
            Ok(()) => log::info!("finder integration removed"),
            Err(e) => log::warn!("finder integration: {e:#}"),
        }
    }
}

/// A menu accelerator showing a hotkey ({win, ctrl, alt, shift} + VK code);
/// `None` for keys a menu can't show.
fn accel_text(win: bool, ctrl: bool, alt: bool, shift: bool, vk: u32) -> Option<String> {
    let key = match vk {
        0x41..=0x5A | 0x30..=0x39 => char::from_u32(vk)?.to_string(),
        0x70..=0x7B => format!("F{}", vk - 0x6F),
        0x09 => "Tab".into(),
        0x20 => "Space".into(),
        0xC0 => "`".into(),
        _ => return None,
    };
    let mut parts = Vec::new();
    for (on, name) in [(ctrl, "Control"), (alt, "Alt"), (shift, "Shift"), (win, "Super")] {
        if on {
            parts.push(name);
        }
    }
    parts.push(&key);
    Some(parts.join("+"))
}

fn set_accel(item: &MenuItem, cur: &mut Option<String>, new: Option<String>) {
    if *cur != new {
        let a = new.as_deref().and_then(|s| s.parse::<tray_icon::menu::accelerator::Accelerator>().ok());
        let _ = item.set_accelerator(a);
        *cur = new;
    }
}

/// What the Tools part needs from the running modules.
struct ToolsSnapshot {
    color: Option<color_picker::Hotkey>,
    ocr: Option<text_extractor::Hotkey>,
    clipboard: Option<ClipState>,
    switcher: Option<window_switcher::Hotkey>,
    shelf: bool,
    /// The menu bar manager runs (and shows its hidden items).
    menubar: Option<bool>,
    displays: Option<Vec<display_control::DisplayInfo>>,
}

fn refresh_tools(t: &mut Tools) {
    let cfg = current_config();
    let Some(s) = try_with_running(|r| ToolsSnapshot {
        color: r.color.as_ref().map(|_| cfg.color_picker.settings.hotkey),
        ocr: r.ocr.as_ref().map(|_| cfg.text_extractor.settings.hotkey),
        clipboard: r.clipboard.as_ref().map(|c| {
            let items = c.recent(CLIP_ITEMS).into_iter().map(|i| (i.id, menu_label(&i))).collect();
            (c.paused(), c.config().hotkey(), items)
        }),
        switcher: r.switcher.as_ref().map(|w| w.config().trigger),
        shelf: r.shelf.is_some(),
        menubar: r.menubar.as_ref().map(|m| m.expansion() != menubar_manager::Expansion::Collapsed),
        displays: r.display.as_ref().map(|d| d.displays()),
    }) else {
        return;
    };

    t.pick_color.set_enabled(s.color.is_some());
    let a = s.color.and_then(|h| accel_text(h.win, h.ctrl, h.alt, h.shift, h.code));
    set_accel(&t.pick_color, &mut t.accels[0], a);
    t.extract_text.set_enabled(s.ocr.is_some());
    let a = s.ocr.and_then(|h| accel_text(h.win, h.ctrl, h.alt, h.shift, h.code));
    set_accel(&t.extract_text, &mut t.accels[1], a);
    t.switcher.set_enabled(s.switcher.is_some());
    let a = s.switcher.and_then(|h| accel_text(h.win, h.ctrl, h.alt, h.shift, h.code));
    set_accel(&t.switcher, &mut t.accels[2], a);
    t.shelf.set_enabled(s.shelf);
    t.menubar_show.set_enabled(s.menubar.is_some());
    t.menubar_show.set_text(if s.menubar == Some(true) { "Hide menu bar items again" } else { "Show hidden menu bar items" });

    // Clipboard history
    match &s.clipboard {
        None => {
            t.clip.set_text("Clipboard history (off)");
            t.clip.set_enabled(false);
            t.clip_shown = None;
        }
        Some((paused, hotkey, items)) => {
            t.clip.set_text(if *paused { "Clipboard history (paused)" } else { "Clipboard history" });
            t.clip.set_enabled(true);
            t.clip_pause.set_text(if *paused { "Resume recording" } else { "Pause recording" });
            t.clip_clear.set_enabled(!items.is_empty());
            let a = hotkey.and_then(|h| {
                use keyboard_manager::keys::Side;
                let on = |s: Side| s != Side::None;
                accel_text(on(h.mods.win), on(h.mods.ctrl), on(h.mods.alt), on(h.mods.shift), h.key)
            });
            set_accel(&t.clip_open, &mut t.accels[3], a);
            if t.clip_shown.as_ref() != Some(items) {
                rebuild_clipboard_menu(t, items);
                t.clip_shown = Some(items.clone());
            }
        }
    }

    // Displays: external ones (the built-in display has its own keys).
    let external: Vec<display_control::DisplayInfo> =
        s.displays.unwrap_or_default().into_iter().filter(|d| !d.is_builtin && d.enabled).collect();
    t.displays.set_enabled(true);
    let sig: Vec<(String, String, bool)> =
        external.iter().map(|d| (d.id.clone(), d.name.clone(), d.volume.is_some())).collect();
    if t.displays_shown.as_ref() != Some(&sig) {
        rebuild_displays_menu(t, &sig);
        t.displays_shown = Some(sig);
    }
    for (id, bright, vol, mute) in &t.display_items {
        let Some(d) = external.iter().find(|d| &d.id == id) else { continue };
        let near = |cur: Option<u8>, preset: u8| cur.is_some_and(|c| c.abs_diff(preset) <= 2);
        let can_dim = d.brightness_method != display_control::BrightnessMethod::None;
        for (item, p) in bright.iter().zip(DISPLAY_PRESETS) {
            item.set_enabled(can_dim);
            item.set_checked(near(d.brightness, p));
        }
        for (item, p) in vol.iter().zip(DISPLAY_PRESETS) {
            item.set_checked(near(d.volume, p) && d.muted != Some(true));
        }
        if let Some(m) = mute {
            m.set_checked(d.muted == Some(true));
        }
    }

    // Finder Quick Actions (cheap file checks on macOS; Windows asks reg.exe,
    // so only after a click there).
    if cfg!(target_os = "macos") && t.finder_checked_at.elapsed() >= Duration::from_secs(10) {
        t.finder_checked_at = std::time::Instant::now();
        t.finder.set_checked(file_tools::finder_integration_installed());
    }
    // Hidden files: `defaults read` (macOS) / the registry, so not too often.
    if t.hidden_checked_at.elapsed() >= Duration::from_secs(15) {
        t.hidden_checked_at = std::time::Instant::now();
        t.hidden_files.set_checked(file_tools::hidden_files_shown());
    }
}

/// "Eject all drives": eject on another thread, then report.
fn eject_all() {
    std::thread::spawn(|| {
        let cfg = crate::CONFIG_PATH
            .get()
            .map(|p| file_tools::Config::load_from(&pm_config::file_tools_path_for(p)))
            .unwrap_or_default()
            .finder;
        let filter = file_tools::finder::volumes::EjectFilter { disk_images: cfg.eject_disk_images, network: cfg.eject_network_volumes };
        let report = file_tools::finder::volumes::eject_all_with(filter);
        let summary = report.summary();
        if report.failed.is_empty() {
            log::info!("eject all: {summary}");
        } else {
            log::warn!("eject all: {summary}");
        }
        notify("PowerMac", &summary);
    });
}

/// The "permissions missing" line, the update line and the icon's dot.
fn refresh_alerts(ui: &mut Ui) {
    // Permission checks are cheap, but not every 2 s while nothing changes.
    if ui.alert_checked_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(4)) {
        ui.alert_checked_at = Some(std::time::Instant::now());
        let missing = crate::app_glue::missing_permissions();
        let text = (!missing.is_empty()).then(|| {
            let names: Vec<&str> = missing.iter().map(|p| p.title()).collect();
            format!("⚠ Needs {} — fix…", names.join(", "))
        });
        if text != ui.alert_shown {
            if ui.alert_shown.is_some() {
                let _ = ui.menu.remove(&ui.alert);
            }
            if let Some(t) = &text {
                ui.alert.set_text(t);
                let _ = ui.menu.insert(&ui.alert, 2);
            }
            ui.alert_shown = text;
        }
    }
    let offer = crate::app_glue::offered_update().map(|r| r.version);
    if offer != ui.update_shown {
        if ui.update_shown.is_some() {
            let _ = ui.menu.remove(&ui.update);
        }
        if let Some(v) = &offer {
            ui.update.set_text(format!("Update available: v{v}…"));
            let _ = ui.menu.insert(&ui.update, 2 + usize::from(ui.alert_shown.is_some()));
        }
        ui.update_shown = offer;
    }
    let badge = ui.alert_shown.is_some();
    if badge != ui.badged {
        ui.badged = badge;
        let img = icon_with(badge);
        #[cfg(target_os = "macos")]
        let _ = ui.tray.set_icon_templated(Some(img));
        #[cfg(not(target_os = "macos"))]
        let _ = ui.tray.set_icon(Some(img));
        let _ = ui.tray.set_tooltip(Some(if badge { "PowerMac — a permission is missing" } else { "PowerMac" }));
    }
}

/// One item per saved workspace (launches it), then capture / edit.
fn refresh_workspaces(w: &mut Workspaces) {
    if w.checked_at.is_some_and(|t| t.elapsed() < Duration::from_secs(4)) && w.shown.is_some() {
        return;
    }
    w.checked_at = Some(std::time::Instant::now());
    let on = current_config().workspaces.enabled;
    let stamp = crate::app_glue::workspaces_stamp();
    let list = crate::app_glue::workspace_list();
    let sig = (stamp, on, list.len());
    if w.shown == Some(sig) {
        return;
    }
    while w.menu.remove_at(0).is_some() {}
    if !on {
        let _ = w.menu.append(&MenuItem::new("Workspaces is off", false, None));
    } else if list.is_empty() {
        let _ = w.menu.append(&MenuItem::new("No workspaces yet", false, None));
    }
    for (id, name) in &list {
        let _ = w.menu.append(&MenuItem::with_id(format!("ws-launch:{id}"), name, on, None));
    }
    let _ = w.menu.append(&PredefinedMenuItem::separator());
    w.capture.set_enabled(on);
    let _ = w.menu.append(&w.capture);
    let _ = w.menu.append(&w.edit);
    w.shown = Some(sig);
}

/// A system notification (macOS: through osascript; elsewhere only logged).
pub(crate) fn notify(title: &str, text: &str) {
    #[cfg(target_os = "macos")]
    {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{}\" with title \"{}\"", esc(text), esc(title));
        if let Ok(mut c) = std::process::Command::new("/usr/bin/osascript").arg("-e").arg(script).spawn() {
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (title, text);
}

// ---------------------------------------------------------------------------
// Sound

/// `out:<uid>`, `in:<uid>`, `app:<pct>:<bundle>` or `mute:<bundle>`.
fn sound_click(rest: &str) {
    let rest = rest.to_string();
    // CoreAudio calls and the RUNNING lock: off the main thread.
    std::thread::spawn(move || {
        let (kind, arg) = rest.split_once(':').unwrap_or(("", ""));
        let r = with_running(|r| {
            let a = r.audio.as_ref()?;
            Some(match kind {
                "out" => a.set_default_output(arg),
                "in" => a.set_default_input(arg),
                "mute" => {
                    let muted = a.app_levels().get(arg).is_some_and(|l| l.muted);
                    a.set_app_mute(arg, !muted)
                }
                "app" => {
                    let (pct, bundle) = arg.split_once(':')?;
                    let pct: f32 = pct.parse().ok()?;
                    let _ = a.set_app_mute(bundle, false);
                    a.set_app_volume(bundle, pct / 100.0)
                }
                _ => return None,
            })
        })
        .flatten();
        if let Some(Err(e)) = r {
            log::warn!("sound: {e:#}");
        }
        crate::audio_glue::refresh_soon();
    });
}

fn refresh_sound(ui: &mut Ui) {
    let latest = crate::audio_glue::latest();
    let sig: SoundSig = latest.as_ref().map(|(st, per_app)| {
        let mut devs: Vec<(char, String, String, bool)> =
            st.outputs.iter().map(|d| ('o', d.uid.clone(), d.name.clone(), d.is_default)).collect();
        devs.extend(st.inputs.iter().map(|d| ('i', d.uid.clone(), d.name.clone(), d.is_default)));
        let apps = per_app.then(|| {
            st.apps
                .iter()
                .map(|a| (a.bundle_id.clone(), a.name.clone(), (a.volume * 100.0).round() as u32, a.muted))
                .collect()
        });
        (devs, apps)
    });
    if ui.sound_shown.as_ref() == Some(&sig) {
        return;
    }
    rebuild_sound_menu(&ui.sound, &sig);
    ui.sound_shown = Some(sig);
}

fn rebuild_sound_menu(menu: &Submenu, sig: &SoundSig) {
    while menu.remove_at(0).is_some() {}
    let Some((devs, apps)) = sig else {
        menu.set_enabled(false);
        menu.set_text("Sound");
        return;
    };
    menu.set_enabled(true);
    let add = |item: &dyn tray_icon::menu::IsMenuItem| {
        let _ = menu.append(item);
    };
    for (kind, title) in [('o', "Output"), ('i', "Input")] {
        if kind == 'i' {
            add(&PredefinedMenuItem::separator());
        }
        add(&MenuItem::new(title, false, None));
        let mut any = false;
        for (k, uid, name, default) in devs.iter().filter(|d| d.0 == kind) {
            let id = format!("snd-{}:{uid}", if *k == 'o' { "out" } else { "in" });
            add(&CheckMenuItem::with_id(id, name, true, *default, None));
            any = true;
        }
        if !any {
            add(&MenuItem::new("None", false, None));
        }
    }
    if let Some(apps) = apps {
        add(&PredefinedMenuItem::separator());
        add(&MenuItem::new("App volume", false, None));
        if apps.is_empty() {
            add(&MenuItem::new("No apps playing", false, None));
        }
        for (bundle, name, pct, muted) in apps {
            let label = if *muted { format!("{name} — muted") } else { format!("{name} — {pct} %") };
            let sub = Submenu::new(label, true);
            for p in APP_PRESETS {
                let on = !muted && pct.abs_diff(p) <= 2;
                let _ = sub.append(&CheckMenuItem::with_id(format!("snd-app:{p}:{bundle}"), format!("{p} %"), true, on, None));
            }
            let _ = sub.append(&PredefinedMenuItem::separator());
            let _ = sub.append(&CheckMenuItem::with_id(format!("snd-mute:{bundle}"), "Mute", true, *muted, None));
            add(&sub);
        }
    }
}

// ---------------------------------------------------------------------------
// Battery

fn refresh_battery(ui: &mut Ui) {
    let Some(snap) = try_with_running(|r| r.battery.as_ref().map(|b| b.status())) else { return };
    let limit_on = current_config().battery_limit.enabled;
    let Some(st) = snap.filter(|s| s.battery.has_battery) else {
        ui.battery.set_text("Battery");
        ui.battery.set_enabled(false);
        return;
    };
    ui.battery.set_enabled(true);
    let pct = st.battery.percent.map(|p| format!("{p} %")).unwrap_or_default();
    ui.battery.set_text(format!("Battery · {pct}"));
    ui.battery_status.set_text(crate::battery_glue::status_line(&st));
    ui.battery_detail.set_text(crate::battery_glue::detail_line(&st));
    let can = crate::battery_glue::can_limit(&st);
    let (text, enabled) = match (can, limit_on) {
        (true, true) if st.top_up_active => ("Charging to 100 % until unplugged".to_string(), false),
        (true, true) if st.needs_helper => ("Charge limit needs the helper (see Settings)".to_string(), false),
        (true, true) => (format!("Charge to 100 % now ({})", st.strategy_label), true),
        (true, false) => ("Charge limit is off".to_string(), false),
        (false, _) => ("No charge limit on this Mac (see Settings)".to_string(), false),
    };
    ui.battery_full.set_text(text);
    ui.battery_full.set_enabled(enabled);
}

/// The config file as it is on disk (for hotkeys the handles don't expose).
fn current_config() -> pm_config::AppConfig {
    crate::CONFIG_PATH
        .get()
        .and_then(|p| pm_config::AppConfig::load_or_default(p).ok())
        .unwrap_or_default()
}

fn menu_label(i: &clipboard_history::ItemSummary) -> String {
    use clipboard_history::Kind;
    let text = i.label(42);
    let text = if text.is_empty() {
        match i.kind {
            Kind::Image => match i.image_size {
                Some((w, h)) => format!("Image {w}×{h}"),
                None => "Image".into(),
            },
            Kind::Files => format!("{} file{}", i.file_count, if i.file_count == 1 { "" } else { "s" }),
            _ => "(empty)".into(),
        }
    } else {
        text
    };
    if i.pinned {
        format!("📌 {text}")
    } else {
        text
    }
}

fn rebuild_clipboard_menu(t: &Tools, items: &[(u64, String)]) {
    while t.clip.remove_at(0).is_some() {}
    let _ = t.clip.append(&t.clip_open);
    let _ = t.clip.append(&PredefinedMenuItem::separator());
    if items.is_empty() {
        let _ = t.clip.append(&MenuItem::new("No items yet", false, None));
    }
    for (id, label) in items {
        let _ = t.clip.append(&MenuItem::with_id(format!("clip:{id}"), label, true, None));
    }
    let _ = t.clip.append(&PredefinedMenuItem::separator());
    let _ = t.clip.append(&t.clip_pause);
    let _ = t.clip.append(&t.clip_clear);
}

fn rebuild_displays_menu(t: &mut Tools, sig: &[(String, String, bool)]) {
    while t.displays.remove_at(0).is_some() {}
    t.display_items.clear();
    if sig.is_empty() {
        let off = with_running(|r| r.display.is_none()).unwrap_or(true);
        let _ = t.displays.append(&MenuItem::new(
            if off { "Display control is off" } else { "No external displays" },
            false,
            None,
        ));
        return;
    }
    let single = sig.len() == 1;
    for (id, name, has_volume) in sig {
        // One display: its controls directly; several: a submenu each.
        let sub = Submenu::new(name, true);
        let target: &dyn Fn(&dyn tray_icon::menu::IsMenuItem) = &|item| {
            let _ = if single { t.displays.append(item) } else { sub.append(item) };
        };
        if single {
            target(&MenuItem::new(name, false, None));
            target(&PredefinedMenuItem::separator());
        }
        target(&MenuItem::new("Brightness", false, None));
        let bright: Vec<CheckMenuItem> = DISPLAY_PRESETS
            .iter()
            .map(|p| CheckMenuItem::with_id(format!("disp:b:{p}:{id}"), format!("{p} %"), true, false, None))
            .collect();
        for b in &bright {
            target(b);
        }
        let mut vol = Vec::new();
        let mut mute = None;
        if *has_volume {
            target(&PredefinedMenuItem::separator());
            target(&MenuItem::new("Volume", false, None));
            vol = DISPLAY_PRESETS
                .iter()
                .map(|p| CheckMenuItem::with_id(format!("disp:v:{p}:{id}"), format!("{p} %"), true, false, None))
                .collect();
            for v in &vol {
                target(v);
            }
            let m = CheckMenuItem::with_id(format!("disp:m:{id}"), "Mute", true, false, None);
            target(&m);
            mute = Some(m);
        }
        if !single {
            let _ = t.displays.append(&sub);
        }
        t.display_items.push((id.clone(), bright, vol, mute));
    }
}
