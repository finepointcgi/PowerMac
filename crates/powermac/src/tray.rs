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
    _tray: TrayIcon,
    mwb_status: MenuItem,
    mwb_switch: Submenu,
    mwb_machines: Vec<String>,
    awake_status: MenuItem,
    awake_off: CheckMenuItem,
    awake_choices: Vec<CheckMenuItem>,
    awake_display: CheckMenuItem,
    zones_status: MenuItem,
    layouts: Vec<CheckMenuItem>,
    copy_key: MenuItem,
    login: CheckMenuItem,
    find_mouse: MenuItem,
    highlighter: CheckMenuItem,
    crosshairs: CheckMenuItem,
    scroll_mouse: CheckMenuItem,
    scroll_trackpad: CheckMenuItem,
    open_privacy: MenuItem,
    open_config: MenuItem,
    quit: MenuItem,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Create the tray icon. Must be called on the main thread, before the loop runs.
pub fn install() -> anyhow::Result<()> {
    let menu = Menu::new();
    let title = MenuItem::new("PowerMac", false, None);

    let mwb_status = MenuItem::new("Mouse Without Borders: off", false, None);
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
    let open_config = MenuItem::new("Open config folder", true, None);
    let login = CheckMenuItem::new("Launch at login", true, crate::login::is_enabled(), None);
    let quit = MenuItem::new("Quit PowerMac", true, None);
    menu.append_items(&[
        &title,
        &PredefinedMenuItem::separator(),
        &mwb_status,
        &mwb_switch,
        &copy_key,
        &PredefinedMenuItem::separator(),
        &awake,
        &zones,
        &scroll,
        &mouse,
        &PredefinedMenuItem::separator(),
        &login,
        &open_privacy,
        &open_config,
        &quit,
    ])?;

    let builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("PowerMac");
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(icon());
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(icon());
    let tray = builder.build()?;

    UI.with(|u| {
        *u.borrow_mut() = Some(Ui {
            _tray: tray,
            mwb_status,
            mwb_switch,
            mwb_machines: Vec::new(),
            awake_status,
            awake_off,
            awake_choices,
            awake_display,
            zones_status,
            layouts,
            copy_key,
            login,
            scroll_mouse,
            scroll_trackpad,
            find_mouse,
            highlighter,
            crosshairs,
            open_privacy,
            open_config,
            quit,
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

fn on_click(ui: &mut Ui, id: &tray_icon::menu::MenuId) {
    if id == ui.quit.id() {
        shutdown();
        std::process::exit(0);
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
            s.enabled = mouse || trackpad || s.reverse_mouse_horizontal || s.reverse_trackpad_horizontal;
        });
        if let Err(e) = r {
            log::warn!("scroll reverser: {e:#}");
        }
        return;
    }
    if id == ui.login.id() {
        if let Err(e) = crate::login::set_enabled(ui.login.is_checked()) {
            log::warn!("launch at login: {e:#}");
        }
        ui.login.set_checked(crate::login::is_enabled());
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
    let snapshot = with_running(|r| {
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
            ui.mwb_switch.set_enabled(false);
        }
        Some(st) => {
            let peers: Vec<_> = st.peers.iter().filter(|p| p.connections > 0).map(|p| p.name.clone()).collect();
            let text = if let Some(by) = &st.controlled_by {
                format!("Controlled by {by}")
            } else if peers.is_empty() {
                "Mouse Without Borders: no peers".to_string()
            } else {
                format!("Connected: {}", peers.join(", "))
            };
            ui.mwb_status.set_text(text);
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

/// 32x32 icon: a screen outline split into three zones. Black (template) on
/// macOS so it follows the menu bar's appearance; blue on Windows.
fn icon() -> Icon {
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
    Icon::from_rgba(px, N as u32, N as u32).expect("valid icon")
}
