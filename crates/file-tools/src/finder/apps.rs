//! Terminals and editors: the known apps, which are installed, and the
//! command that opens a folder in one.

use std::path::{Path, PathBuf};

/// A terminal or an editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppKind {
    Terminal,
    Editor,
}

/// How a known app opens a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launch {
    /// `open -b <id> <dir>` (the app handles a dropped / opened folder).
    Open,
    /// `open -n -b <id> --args <args…> <dir>` with the dir after the
    /// given flag (`--working-directory`, `--directory`, `start --cwd`).
    NewInstance(&'static [&'static str]),
    /// Windows: run a command line (`{dir}` is replaced).
    Command(&'static [&'static str]),
}

/// A terminal or editor PowerMac knows how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownApp {
    /// macOS bundle id, or the Windows command / short name.
    pub id: &'static str,
    pub name: &'static str,
    pub kind: AppKind,
    launch: Launch,
    /// Windows: the executable looked up on PATH to tell it's installed.
    probe: &'static str,
}

const fn mac(id: &'static str, name: &'static str, kind: AppKind, launch: Launch) -> KnownApp {
    KnownApp {
        id,
        name,
        kind,
        launch,
        probe: "",
    }
}

const fn win(
    id: &'static str,
    name: &'static str,
    kind: AppKind,
    probe: &'static str,
    cmd: &'static [&'static str],
) -> KnownApp {
    KnownApp {
        id,
        name,
        kind,
        launch: Launch::Command(cmd),
        probe,
    }
}

use AppKind::{Editor, Terminal};

/// macOS terminals and editors, in the order "auto" picks them.
pub const MAC_APPS: &[KnownApp] = &[
    mac("com.apple.Terminal", "Terminal", Terminal, Launch::Open),
    mac("com.googlecode.iterm2", "iTerm", Terminal, Launch::Open),
    mac("dev.warp.Warp-Stable", "Warp", Terminal, Launch::Open),
    mac("com.mitchellh.ghostty", "Ghostty", Terminal, Launch::Open),
    mac(
        "net.kovidgoyal.kitty",
        "kitty",
        Terminal,
        Launch::NewInstance(&["--single-instance", "--directory"]),
    ),
    mac(
        "org.alacritty",
        "Alacritty",
        Terminal,
        Launch::NewInstance(&["--working-directory"]),
    ),
    mac(
        "com.github.wez.wezterm",
        "WezTerm",
        Terminal,
        Launch::NewInstance(&["start", "--cwd"]),
    ),
    mac("com.microsoft.VSCode", "VS Code", Editor, Launch::Open),
    mac(
        "com.todesktop.230313mt0aa7e5",
        "Cursor",
        Editor,
        Launch::Open,
    ),
    mac("dev.zed.Zed", "Zed", Editor, Launch::Open),
    mac("com.sublimetext.4", "Sublime Text", Editor, Launch::Open),
    mac("com.exafunction.windsurf", "Windsurf", Editor, Launch::Open),
    mac(
        "com.microsoft.VSCodeInsiders",
        "VS Code Insiders",
        Editor,
        Launch::Open,
    ),
    mac("com.vscodium", "VSCodium", Editor, Launch::Open),
    mac("com.panic.Nova", "Nova", Editor, Launch::Open),
    mac("com.barebones.bbedit", "BBEdit", Editor, Launch::Open),
    mac("com.sublimetext.3", "Sublime Text 3", Editor, Launch::Open),
    mac("com.apple.dt.Xcode", "Xcode", Editor, Launch::Open),
];

/// Windows terminals and editors (`{dir}` is the folder).
pub const WINDOWS_APPS: &[KnownApp] = &[
    win(
        "wt",
        "Windows Terminal",
        Terminal,
        "wt.exe",
        &["wt.exe", "-d", "{dir}"],
    ),
    win(
        "pwsh",
        "PowerShell 7",
        Terminal,
        "pwsh.exe",
        &[
            "cmd.exe", "/C", "start", "", "/D", "{dir}", "pwsh.exe", "-NoExit",
        ],
    ),
    win(
        "powershell",
        "Windows PowerShell",
        Terminal,
        "powershell.exe",
        &[
            "cmd.exe",
            "/C",
            "start",
            "",
            "/D",
            "{dir}",
            "powershell.exe",
            "-NoExit",
        ],
    ),
    win(
        "cmd",
        "Command Prompt",
        Terminal,
        "cmd.exe",
        &["cmd.exe", "/C", "start", "", "/D", "{dir}", "cmd.exe"],
    ),
    win(
        "code",
        "VS Code",
        Editor,
        "code.cmd",
        &["cmd.exe", "/C", "code", "{dir}"],
    ),
    win(
        "cursor",
        "Cursor",
        Editor,
        "cursor.cmd",
        &["cmd.exe", "/C", "cursor", "{dir}"],
    ),
    win("zed", "Zed", Editor, "zed.exe", &["zed.exe", "{dir}"]),
    win(
        "subl",
        "Sublime Text",
        Editor,
        "subl.exe",
        &["subl.exe", "{dir}"],
    ),
    win(
        "windsurf",
        "Windsurf",
        Editor,
        "windsurf.cmd",
        &["cmd.exe", "/C", "windsurf", "{dir}"],
    ),
];

/// The known apps of this platform.
pub fn known_apps() -> &'static [KnownApp] {
    if cfg!(windows) {
        WINDOWS_APPS
    } else {
        MAC_APPS
    }
}

/// A known app by id (case-insensitive).
pub fn find_known(id: &str) -> Option<&'static KnownApp> {
    known_apps()
        .iter()
        .chain(MAC_APPS)
        .chain(WINDOWS_APPS)
        .find(|a| a.id.eq_ignore_ascii_case(id))
}

/// An installed terminal / editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub kind: AppKind,
    /// The .app bundle (macOS) or the executable (Windows), when found.
    pub path: Option<PathBuf>,
}

/// The installed apps of `kind`, in preference order.
pub fn installed_apps(kind: AppKind) -> Vec<InstalledApp> {
    known_apps()
        .iter()
        .filter(|a| a.kind == kind)
        .filter_map(|a| {
            app_location(a).map(|p| InstalledApp {
                id: a.id.into(),
                name: a.name.into(),
                kind,
                path: Some(p),
            })
        })
        .collect()
}

fn app_location(a: &KnownApp) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let _ = a.probe;
        bundle_path(a.id)
    }
    #[cfg(not(target_os = "macos"))]
    {
        find_on_path(a.probe)
    }
}

/// The .app of `bundle_id` (Launch Services, via NSWorkspace).
#[cfg(target_os = "macos")]
pub fn bundle_path(bundle_id: &str) -> Option<PathBuf> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::NSString;
    objc2::rc::autoreleasepool(|_| {
        let ws = NSWorkspace::sharedWorkspace();
        let url = ws.URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))?;
        let p = url.path()?;
        Some(PathBuf::from(p.to_string()))
    })
}

/// `exe` in a folder on PATH (or in the usual WindowsApps / Program Files
/// spots for wt / code).
#[cfg(not(target_os = "macos"))]
pub fn find_on_path(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for d in std::env::split_paths(&path) {
        let p = d.join(exe);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        for p in [
            local.join("Microsoft/WindowsApps").join(exe),
            local.join("Programs/Microsoft VS Code/bin").join(exe),
            local.join("Programs/cursor/resources/app/bin").join(exe),
        ] {
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// The app for `kind` given the configured choice (empty: the first
/// installed one, else the platform default).
pub fn resolve_app(kind: AppKind, configured: &str) -> String {
    let c = configured.trim();
    if !c.is_empty() {
        return c.to_string();
    }
    if let Some(a) = installed_apps(kind).into_iter().next() {
        return a.id;
    }
    default_app(kind).to_string()
}

/// The fallback when nothing is configured or detected.
pub fn default_app(kind: AppKind) -> &'static str {
    match (kind, cfg!(windows)) {
        (Terminal, false) => "com.apple.Terminal",
        (Editor, false) => "com.apple.TextEdit",
        (Terminal, true) => "powershell",
        (Editor, true) => "notepad",
    }
}

/// A display name for an app id (known name, an .app's name, or the id).
pub fn app_name(id: &str) -> String {
    if let Some(a) = find_known(id) {
        return a.name.to_string();
    }
    if id == "com.apple.TextEdit" {
        return "TextEdit".into();
    }
    let p = Path::new(id);
    if p.extension().is_some_and(|e| e == "app") {
        if let Some(s) = p.file_stem() {
            return s.to_string_lossy().into_owned();
        }
    }
    id.to_string()
}

/// The command (program first) that opens `dir` in `app` on this
/// platform. `app` is a known id, a bundle id, an `.app` path, or (Windows)
/// a command.
pub fn open_command(app: &str, dir: &Path) -> Vec<String> {
    open_command_for(app, dir, cfg!(windows))
}

/// [`open_command`] for macOS (`windows == false`) or Windows.
pub fn open_command_for(app: &str, dir: &Path, windows: bool) -> Vec<String> {
    let d = dir.to_string_lossy().into_owned();
    let known = if windows {
        WINDOWS_APPS.iter().find(|a| a.id.eq_ignore_ascii_case(app))
    } else {
        MAC_APPS.iter().find(|a| a.id.eq_ignore_ascii_case(app))
    };
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    if windows {
        if let Some(KnownApp {
            launch: Launch::Command(cmd),
            ..
        }) = known
        {
            return cmd.iter().map(|a| a.replace("{dir}", &d)).collect();
        }
        // A custom command or exe: run it with the folder.
        return vec![app.to_string(), d];
    }
    match known.map(|a| a.launch) {
        Some(Launch::NewInstance(args)) => {
            let mut v = s(&["/usr/bin/open", "-n", "-b", app, "--args"]);
            v.extend(args.iter().map(|a| a.to_string()));
            v.push(d);
            v
        }
        _ if app.contains('/') || app.ends_with(".app") => s(&["/usr/bin/open", "-a", app, &d]),
        _ => s(&["/usr/bin/open", "-b", app, &d]),
    }
}

/// The folder to open for a selected item: itself if a folder, else its
/// parent.
pub fn folder_of(p: &Path) -> PathBuf {
    let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    if p.is_dir() {
        p
    } else {
        p.parent().map(Path::to_path_buf).unwrap_or(p)
    }
}

/// The distinct folders of `paths` (in order, at most `max`).
pub fn folders_of(paths: &[PathBuf], max: usize) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        let f = folder_of(p);
        if !out.contains(&f) {
            out.push(f);
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Run an open command without waiting for the app.
pub fn spawn(cmd: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(!cmd.is_empty(), "empty command");
    let mut c = std::process::Command::new(&cmd[0]);
    c.args(&cmd[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    #[cfg(target_os = "macos")]
    {
        // `open` returns once Launch Services has the request; report its errors.
        let out = c.output()?;
        anyhow::ensure!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        c.stderr(std::process::Stdio::null()).spawn()?;
        Ok(())
    }
}
