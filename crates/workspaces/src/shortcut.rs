//! Desktop shortcuts that launch a workspace
//! (`powermac workspace launch <id>`).
//!
//! macOS: a tiny app bundle whose executable is a shell script (no Terminal
//! window, no Dock icon; it borrows PowerMac's icon when PowerMac runs from
//! its .app), or a `.command` file (opens Terminal). Windows: a `.lnk`
//! made with PowerShell's `WScript.Shell`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShortcutKind {
    #[default]
    App,
    Command,
}

/// The user's desktop folder.
pub fn desktop_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Desktop"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Desktop"))
    }
}

/// The `powermac` binary shortcuts run: this executable.
pub fn default_exe() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.canonicalize().ok()).unwrap_or_else(|| PathBuf::from("powermac"))
}

/// A file name for the workspace (no path separators or reserved chars).
pub fn file_stem(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '-' } else { c })
        .collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() {
        "Workspace".into()
    } else {
        s
    }
}

/// `'…'` for /bin/sh.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The shell script both macOS shortcut kinds run.
pub fn launch_script(ws: &Workspace, exe: &Path) -> String {
    format!(
        "#!/bin/sh\n# PowerMac Workspaces shortcut for \"{}\".\nexec {} workspace launch {}\n",
        ws.name.replace('\n', " "),
        sh_quote(&exe.to_string_lossy()),
        sh_quote(&ws.id)
    )
}

#[cfg(unix)]
fn make_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_p: &Path) -> Result<()> {
    Ok(())
}

/// The `.app` bundle `exe` lives in, if any.
fn enclosing_app(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && app.extension()? == "app").then(|| app.to_path_buf())
}

/// Create a shortcut for `ws` in `dir`. Returns its path. An existing
/// shortcut of the same name is replaced.
pub fn create(ws: &Workspace, dir: &Path, exe: &Path, kind: ShortcutKind) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let stem = file_stem(&ws.name);
    if cfg!(windows) {
        return create_lnk(ws, dir, exe, &stem);
    }
    match kind {
        ShortcutKind::Command => {
            let p = dir.join(format!("{stem}.command"));
            std::fs::write(&p, launch_script(ws, exe)).with_context(|| format!("writing {}", p.display()))?;
            make_executable(&p)?;
            Ok(p)
        }
        ShortcutKind::App => {
            let app = dir.join(format!("{stem}.app"));
            if app.exists() {
                // Only replace what we made before.
                let marker = app.join("Contents/MacOS/launch");
                if !marker.exists() {
                    bail!("{} exists and is not a workspace shortcut", app.display());
                }
                std::fs::remove_dir_all(&app)?;
            }
            let macos = app.join("Contents/MacOS");
            std::fs::create_dir_all(&macos)?;
            let script = macos.join("launch");
            std::fs::write(&script, launch_script(ws, exe))?;
            make_executable(&script)?;
            let mut icon_key = String::new();
            if let Some(src_app) = enclosing_app(exe) {
                let res = src_app.join("Contents/Resources");
                if let Some(icns) = std::fs::read_dir(&res).ok().and_then(|rd| {
                    rd.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "icns"))
                }) {
                    let dst = app.join("Contents/Resources");
                    std::fs::create_dir_all(&dst)?;
                    if std::fs::copy(&icns, dst.join("icon.icns")).is_ok() {
                        icon_key = "    <key>CFBundleIconFile</key>\n    <string>icon</string>\n".into();
                    }
                }
            }
            let id_part: String = ws.id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let plist = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>launch</string>
    <key>CFBundleIdentifier</key>
    <string>com.powermac.workspace.{id}</string>
    <key>CFBundleName</key>
    <string>{name}</string>
    <key>CFBundleDisplayName</key>
    <string>{name}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>LSUIElement</key>
    <true/>
{icon}</dict>
</plist>
"#,
                id = id_part,
                name = xml_escape(&ws.name),
                icon = icon_key
            );
            std::fs::write(app.join("Contents/Info.plist"), plist)?;
            Ok(app)
        }
    }
}

/// PowerShell single-quoted string.
pub fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The PowerShell that writes the `.lnk`.
pub fn lnk_script(ws: &Workspace, lnk: &Path, exe: &Path) -> String {
    let args = format!("workspace launch \"{}\"", ws.id);
    format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut({}); $s.TargetPath = {}; $s.Arguments = {}; $s.Description = {}; $s.IconLocation = {}; $s.Save()",
        ps_quote(&lnk.to_string_lossy()),
        ps_quote(&exe.to_string_lossy()),
        ps_quote(&args),
        ps_quote(&format!("Launch the {} workspace", ws.name)),
        ps_quote(&format!("{},0", exe.to_string_lossy())),
    )
}

fn create_lnk(ws: &Workspace, dir: &Path, exe: &Path, stem: &str) -> Result<PathBuf> {
    let lnk = dir.join(format!("{stem}.lnk"));
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &lnk_script(ws, &lnk, exe)]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().context("running PowerShell")?;
    if !out.status.success() {
        bail!("PowerShell failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(lnk)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> Workspace {
        Workspace { id: "{AB-12}".into(), name: "Bob's / Work".into(), ..Default::default() }
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert_eq!(file_stem("a/b:c"), "a-b-c");
        assert_eq!(file_stem("  "), "Workspace");
        let s = launch_script(&ws(), Path::new("/Applications/PowerMac.app/Contents/MacOS/powermac"));
        assert!(s.starts_with("#!/bin/sh\n"));
        assert!(s.contains("exec '/Applications/PowerMac.app/Contents/MacOS/powermac' workspace launch '{AB-12}'"), "{s}");
        let l = lnk_script(&ws(), Path::new(r"C:\Users\b\Desktop\x.lnk"), Path::new(r"C:\PM\powermac.exe"));
        assert!(l.contains(r#"$s.Arguments = 'workspace launch "{AB-12}"'"#), "{l}");
        assert!(l.contains("Bob''s"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn creates_app_and_command_shortcuts() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("ws-shortcut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let exe = Path::new("/usr/local/bin/powermac");
        let app = create(&ws(), &d, exe, ShortcutKind::App).unwrap();
        assert_eq!(app.file_name().unwrap(), "Bob's - Work.app");
        let script = app.join("Contents/MacOS/launch");
        assert_eq!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o777, 0o755);
        let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
        assert!(plist.contains("<string>com.powermac.workspace.AB12</string>"));
        assert!(plist.contains("<key>LSUIElement</key>"));
        // replacing works
        create(&ws(), &d, exe, ShortcutKind::App).unwrap();
        let cmd = create(&ws(), &d, exe, ShortcutKind::Command).unwrap();
        assert!(std::fs::read_to_string(&cmd).unwrap().contains("workspace launch '{AB-12}'"));
        // plutil accepts the plist
        let ok = std::process::Command::new("plutil").arg("-lint").arg(app.join("Contents/Info.plist")).status().map(|s| s.success()).unwrap_or(true);
        assert!(ok);
        let _ = std::fs::remove_dir_all(&d);
    }
}
