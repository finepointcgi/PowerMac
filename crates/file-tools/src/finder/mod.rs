//! "Finder is missing basics": Open in Terminal / editor, Copy path,
//! show / hide hidden files, Eject all external volumes, and "what is using
//! this file / volume".
//!
//! These are library calls for the tray app and settings, plus
//! [`run_cli`] behind `powermac finder <action> <paths…>`, which the Finder
//! Quick Actions (see [`crate::integration`]) and Explorer verbs run.
//!
//! **Permissions (macOS):** none for the basics. Acting on "the front
//! Finder window" without a selection ([`front_finder_folder`],
//! [`front_finder_selection`]) asks Finder through AppleScript, which needs
//! the Automation permission for Finder (macOS asks once, then System
//! Settings ▸ Privacy & Security ▸ Automation). It is opt-in
//! (`finder.useFrontFinderWindow`, or `--front-window` on the command line).

pub mod apps;
pub mod paths;
pub mod volumes;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _};

pub use apps::{
    app_name, installed_apps, open_command, resolve_app, AppKind, InstalledApp, KnownApp,
};
pub use paths::{format_path, format_path_with, format_paths, CopyStyle};
pub use volumes::{
    eject_all, eject_all_with, eject_volume, external_volumes, external_volumes_with, who_is_using,
    EjectFailure, EjectFilter, EjectReport, ProcessInfo, VolumeInfo,
};

use crate::config::{Config, FinderConfig};

// ---------------------------------------------------------------------------
// Open in Terminal / editor

fn finder_cfg() -> FinderConfig {
    Config::load().finder
}

/// Open `dir` (or the folder of a file) in the configured terminal.
pub fn open_in_terminal(dir: &Path) -> anyhow::Result<()> {
    open_in_terminal_with(dir, &finder_cfg().terminal_app)
}

/// Open `dir` in `app` (a bundle id / .app path / Windows id; empty: auto).
pub fn open_in_terminal_with(dir: &Path, app: &str) -> anyhow::Result<()> {
    let app = resolve_app(AppKind::Terminal, app);
    let dir = apps::folder_of(dir);
    apps::spawn(&open_command(&app, &dir))
        .with_context(|| format!("couldn't open {} in {}", dir.display(), app_name(&app)))
}

/// Open `dir` (or the folder of a file) in the configured editor.
pub fn open_in_editor(dir: &Path) -> anyhow::Result<()> {
    open_in_editor_with(dir, &finder_cfg().editor_app)
}

/// Open `dir` in the editor `app` (empty: the first installed one).
pub fn open_in_editor_with(dir: &Path, app: &str) -> anyhow::Result<()> {
    let app = resolve_app(AppKind::Editor, app);
    let dir = apps::folder_of(dir);
    apps::spawn(&open_command(&app, &dir))
        .with_context(|| format!("couldn't open {} in {}", dir.display(), app_name(&app)))
}

// ---------------------------------------------------------------------------
// Copy path

/// Put `paths` (one per line, formatted with `style`) on the clipboard.
/// Returns the copied text.
pub fn copy_paths(paths: &[PathBuf], style: CopyStyle) -> anyhow::Result<String> {
    anyhow::ensure!(!paths.is_empty(), "nothing selected");
    let text = format_paths(paths, style);
    set_clipboard_text(&text)?;
    Ok(text)
}

/// Replace the clipboard with plain text.
pub fn set_clipboard_text(text: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::NSString;
        objc2::rc::autoreleasepool(|_| {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            let ok =
                pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
            anyhow::ensure!(ok, "couldn't write to the clipboard");
            Ok(())
        })
    }
    #[cfg(windows)]
    {
        win_clipboard::set_text(text)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = text;
        bail!("no clipboard on this platform")
    }
}

#[cfg(windows)]
mod win_clipboard {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };

    const CF_UNICODETEXT: u32 = 13;

    pub fn set_text(text: &str) -> anyhow::Result<()> {
        let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let mut opened = false;
            for _ in 0..10 {
                if OpenClipboard(std::ptr::null_mut()) != 0 {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            anyhow::ensure!(opened, "the clipboard is busy");
            EmptyClipboard();
            let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2);
            if h.is_null() {
                CloseClipboard();
                anyhow::bail!("out of memory");
            }
            let p = GlobalLock(h) as *mut u16;
            std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
            GlobalUnlock(h);
            let ok = !SetClipboardData(CF_UNICODETEXT, h as _).is_null();
            CloseClipboard();
            anyhow::ensure!(ok, "couldn't write to the clipboard");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Hidden files

/// What toggling hidden files does, for a confirmation / tooltip.
pub const HIDDEN_FILES_NOTE: &str = "Finder restarts to apply this (open Finder windows reopen). \
Finder also has a built-in shortcut that shows hidden files in the front window: ⌘⇧. (Cmd+Shift+Period).";

/// The commands that make Finder show (`true`) or hide hidden files.
pub fn hidden_files_commands(show: bool) -> Vec<Vec<String>> {
    if cfg!(windows) {
        let v = if show { "1" } else { "2" };
        return vec![[
            "reg",
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
            "/v",
            "Hidden",
            "/t",
            "REG_DWORD",
            "/d",
            v,
            "/f",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()];
    }
    vec![
        vec![
            "/usr/bin/defaults".into(),
            "write".into(),
            "com.apple.finder".into(),
            "AppleShowAllFiles".into(),
            "-bool".into(),
            if show { "YES" } else { "NO" }.into(),
        ],
        vec!["/usr/bin/killall".into(), "Finder".into()],
    ]
}

/// Parse `defaults read com.apple.finder AppleShowAllFiles` output.
pub fn parse_defaults_bool(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "yes" | "true")
}

/// Whether Finder (Explorer) shows hidden files. Reading doesn't restart
/// anything.
pub fn hidden_files_shown() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/defaults")
            .args(["read", "com.apple.finder", "AppleShowAllFiles"])
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| o.status.success() && parse_defaults_bool(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
                "/v",
                "Hidden",
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("0x1"))
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    false
}

/// Show or hide hidden files. **Restarts Finder** on macOS (see
/// [`HIDDEN_FILES_NOTE`]); on Windows, open Explorer windows pick it up
/// after a refresh (F5).
pub fn set_hidden_files_shown(show: bool) -> anyhow::Result<()> {
    for c in hidden_files_commands(show) {
        let mut cmd = std::process::Command::new(&c[0]);
        cmd.args(&c[1..])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let st = cmd.status()?;
        // killall fails harmlessly when Finder isn't running.
        if !st.success() && !c[0].ends_with("killall") {
            bail!("{} failed", c.join(" "));
        }
    }
    Ok(())
}

/// Flip hidden files; returns the new state (`true` = shown).
pub fn toggle_hidden_files() -> anyhow::Result<bool> {
    let show = !hidden_files_shown();
    set_hidden_files_shown(show)?;
    Ok(show)
}

// ---------------------------------------------------------------------------
// The front Finder window (opt-in, needs Automation permission)

/// AppleScript returning the POSIX path of the front Finder window's
/// folder (the Desktop when no window is open).
pub const FRONT_FOLDER_SCRIPT: &str = r#"tell application "Finder"
	if (count of Finder windows) is 0 then return POSIX path of (desktop as alias)
	return POSIX path of (target of front Finder window as alias)
end tell"#;

/// AppleScript returning the selected items' POSIX paths, one per line.
pub const SELECTION_SCRIPT: &str = r#"set out to ""
tell application "Finder"
	repeat with i in (get selection as alias list)
		set out to out & POSIX path of i & linefeed
	end repeat
end tell
return out"#;

fn osascript(script: &str) -> anyhow::Result<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", script])
            .stdin(std::process::Stdio::null())
            .output()?;
        if !out.status.success() {
            let e = String::from_utf8_lossy(&out.stderr);
            if e.contains("-1743") || e.contains("Not authorized") {
                bail!(
                    "PowerMac isn't allowed to control Finder. Allow it in System Settings ▸ \
                     Privacy & Security ▸ Automation."
                );
            }
            bail!("Finder: {}", e.trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = script;
        bail!("only available on macOS")
    }
}

/// The folder shown in the front Finder window (Automation permission).
pub fn front_finder_folder() -> anyhow::Result<PathBuf> {
    let s = osascript(FRONT_FOLDER_SCRIPT)?;
    let p = s.trim_end_matches(['\n', '\r']);
    anyhow::ensure!(!p.is_empty(), "Finder has no folder open");
    Ok(PathBuf::from(p))
}

/// The items selected in Finder (Automation permission).
pub fn front_finder_selection() -> anyhow::Result<Vec<PathBuf>> {
    Ok(osascript(SELECTION_SCRIPT)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let t = l.trim_end_matches(['\r', '\n']);
            PathBuf::from(if t.len() > 1 {
                t.trim_end_matches('/')
            } else {
                t
            })
        })
        .collect())
}

// ---------------------------------------------------------------------------
// CLI: `powermac finder <action> [options] <paths…>`

/// The actions of `powermac finder`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinderAction {
    OpenTerminal,
    OpenEditor,
    CopyPath,
    ToggleHidden,
    ShowHidden,
    HideHidden,
    HiddenStatus,
    Volumes,
    EjectAll,
    WhoUses,
    Apps,
}

impl FinderAction {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open-terminal" | "terminal" => Self::OpenTerminal,
            "open-editor" | "editor" => Self::OpenEditor,
            "copy-path" | "copy-paths" => Self::CopyPath,
            "toggle-hidden" => Self::ToggleHidden,
            "show-hidden" => Self::ShowHidden,
            "hide-hidden" => Self::HideHidden,
            "hidden-status" => Self::HiddenStatus,
            "volumes" => Self::Volumes,
            "eject-all" => Self::EjectAll,
            "who-uses" | "who-is-using" => Self::WhoUses,
            "apps" => Self::Apps,
            _ => return None,
        })
    }
}

/// Usage text of `powermac finder`.
pub const USAGE: &str = "usage: powermac finder <action> [options] [paths…]
  open-terminal [--app <id>] [paths…]   open the folder (of each item) in the terminal
  open-editor   [--app <id>] [paths…]   open the folder in the editor
  copy-path     [--style posix|tilde|url|name|quoted] [paths…]   copy paths, one per line
  toggle-hidden | show-hidden | hide-hidden   (restarts Finder) | hidden-status
  volumes [--all]        list external volumes (--all: every mount)
  eject-all [--dry-run] [--no-images] [--network]
  who-uses <path>        processes using a file, folder or volume
  apps                   detected terminals and editors
  --front-window         without paths, use Finder's selection / front window
                         (needs the Automation permission for Finder)";

/// A parsed `powermac finder` command line.
#[derive(Debug, Clone, PartialEq)]
pub struct FinderCli {
    pub action: FinderAction,
    pub paths: Vec<PathBuf>,
    pub app: Option<String>,
    pub style: Option<CopyStyle>,
    pub front_window: bool,
    pub dry_run: bool,
    pub all: bool,
    pub filter: Option<EjectFilter>,
}

/// Parse the arguments after `finder`.
pub fn parse_cli(args: &[String]) -> anyhow::Result<FinderCli> {
    let mut it = args.iter();
    let a = it.next().with_context(|| USAGE.to_string())?;
    let action = FinderAction::parse(a).with_context(|| format!("unknown action {a}\n{USAGE}"))?;
    let mut c = FinderCli {
        action,
        paths: Vec::new(),
        app: None,
        style: None,
        front_window: false,
        dry_run: false,
        all: false,
        filter: None,
    };
    let mut filter = EjectFilter::default();
    let mut filter_set = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--app" => c.app = Some(it.next().context("--app needs a value")?.clone()),
            "--style" => {
                let v = it.next().context("--style needs a value")?;
                c.style = Some(CopyStyle::parse(v).with_context(|| format!("unknown style {v}"))?);
            }
            "--front-window" => c.front_window = true,
            "--dry-run" | "-n" => c.dry_run = true,
            "--all" => c.all = true,
            "--no-images" => {
                filter.disk_images = false;
                filter_set = true;
            }
            "--network" => {
                filter.network = true;
                filter_set = true;
            }
            "--" => c.paths.extend(it.by_ref().map(PathBuf::from)),
            s if s.starts_with("--") => bail!("unknown option {s}\n{USAGE}"),
            s => c.paths.push(PathBuf::from(s)),
        }
    }
    if filter_set {
        c.filter = Some(filter);
    }
    Ok(c)
}

fn describe_volume(v: &VolumeInfo) -> String {
    let mut kind = Vec::new();
    if v.network {
        kind.push("network");
    }
    if v.disk_image {
        kind.push("disk image");
    }
    if !v.internal && !v.network {
        kind.push("external");
    }
    if v.removable {
        kind.push("removable");
    }
    if v.root {
        kind.push("boot");
    }
    format!(
        "{}\t{}\t{}\t{}\t{}",
        v.name,
        v.mount_point.display(),
        if v.whole_disk.is_empty() {
            &v.device
        } else {
            &v.whole_disk
        },
        volumes::human_size(v.size),
        kind.join(", ")
    )
}

fn describe_procs(p: &[ProcessInfo]) -> String {
    p.iter()
        .map(|p| match &p.bundle_id {
            Some(b) => format!("{}\t{}\t{b}", p.pid, p.name),
            None => format!("{}\t{}", p.pid, p.name),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run `powermac finder <args>` with the settings in `file-tools.json`.
/// Returns what to print.
pub fn run_cli(args: &[String]) -> anyhow::Result<String> {
    run_cli_with(args, &Config::load().finder)
}

/// [`run_cli`] with explicit settings.
pub fn run_cli_with(args: &[String], cfg: &FinderConfig) -> anyhow::Result<String> {
    let c = parse_cli(args)?;
    let front = c.front_window || cfg.use_front_finder_window;
    let selection = |c: &FinderCli| -> anyhow::Result<Vec<PathBuf>> {
        if !c.paths.is_empty() {
            return Ok(c.paths.clone());
        }
        if front {
            let s = front_finder_selection()?;
            if !s.is_empty() {
                return Ok(s);
            }
            return Ok(vec![front_finder_folder()?]);
        }
        bail!(
            "no items given. Pass paths, or allow using the front Finder window \
             (--front-window; needs the Automation permission for Finder)"
        )
    };
    match c.action {
        FinderAction::OpenTerminal | FinderAction::OpenEditor => {
            let items = selection(&c)?;
            let terminal = c.action == FinderAction::OpenTerminal;
            let configured = c.app.clone().unwrap_or_else(|| {
                if terminal {
                    cfg.terminal_app.clone()
                } else {
                    cfg.editor_app.clone()
                }
            });
            let mut opened = Vec::new();
            for d in apps::folders_of(&items, 5) {
                if terminal {
                    open_in_terminal_with(&d, &configured)?;
                } else {
                    open_in_editor_with(&d, &configured)?;
                }
                opened.push(d.display().to_string());
            }
            Ok(opened.join("\n"))
        }
        FinderAction::CopyPath => {
            let items = selection(&c)?;
            copy_paths(&items, c.style.unwrap_or(cfg.path_style))
        }
        FinderAction::ToggleHidden => Ok(if toggle_hidden_files()? {
            "Hidden files shown".into()
        } else {
            "Hidden files hidden".into()
        }),
        FinderAction::ShowHidden | FinderAction::HideHidden => {
            let show = c.action == FinderAction::ShowHidden;
            set_hidden_files_shown(show)?;
            Ok(if show {
                "Hidden files shown"
            } else {
                "Hidden files hidden"
            }
            .into())
        }
        FinderAction::HiddenStatus => Ok(if hidden_files_shown() {
            "shown"
        } else {
            "hidden"
        }
        .into()),
        FinderAction::Volumes => {
            let vols = if c.all {
                volumes::all_volumes()
            } else {
                external_volumes_with(c.filter.unwrap_or(EjectFilter {
                    disk_images: cfg.eject_disk_images,
                    network: cfg.eject_network_volumes,
                }))
            };
            Ok(vols
                .iter()
                .map(describe_volume)
                .collect::<Vec<_>>()
                .join("\n"))
        }
        FinderAction::EjectAll => {
            let f = c.filter.unwrap_or(EjectFilter {
                disk_images: cfg.eject_disk_images,
                network: cfg.eject_network_volumes,
            });
            if c.dry_run {
                let vols = external_volumes_with(f);
                return Ok(volumes::eject_groups(vols)
                    .iter()
                    .map(|g| {
                        let cmd = volumes::eject_command(&g[0]);
                        let names: Vec<&str> = g.iter().map(|v| v.name.as_str()).collect();
                        format!("{}\t{}", names.join(", "), cmd.join(" "))
                    })
                    .collect::<Vec<_>>()
                    .join("\n"));
            }
            let rep = eject_all_with(f);
            let mut s = rep.summary();
            for f in &rep.failed {
                if !f.blockers.is_empty() {
                    s.push_str(&format!(
                        "\n{}:\n{}",
                        f.volume.name,
                        describe_procs(&f.blockers)
                    ));
                }
            }
            if rep.failed.is_empty() {
                Ok(s)
            } else {
                bail!("{s}")
            }
        }
        FinderAction::WhoUses => {
            let p = c.paths.first().context("who-uses needs a path")?;
            Ok(describe_procs(&who_is_using(p)))
        }
        FinderAction::Apps => {
            let mut out = Vec::new();
            for (k, label, conf) in [
                (AppKind::Terminal, "terminal", &cfg.terminal_app),
                (AppKind::Editor, "editor", &cfg.editor_app),
            ] {
                out.push(format!(
                    "{label}: {} (uses {})",
                    if conf.is_empty() { "auto" } else { conf },
                    app_name(&resolve_app(k, conf))
                ));
                for a in installed_apps(k) {
                    out.push(format!("  {}\t{}", a.id, a.name));
                }
            }
            Ok(out.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests;
