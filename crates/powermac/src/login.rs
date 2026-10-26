//! "Launch at login": a LaunchAgent plist on macOS, the HKCU `Run` key on Windows.

use anyhow::{Context, Result};

#[cfg(target_os = "macos")]
const LABEL: &str = "com.powermac.app";

#[cfg(target_os = "macos")]
fn plist_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

pub fn is_enabled() -> bool {
    #[cfg(target_os = "macos")]
    return plist_path().is_some_and(|p| p.exists());
    #[cfg(windows)]
    return std::process::Command::new("reg")
        .args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "PowerMac"])
        .output()
        .is_ok_and(|o| o.status.success());
    #[allow(unreachable_code)]
    false
}

pub fn set_enabled(on: bool) -> Result<()> {
    let exe = std::env::current_exe().context("locating the executable")?;
    #[cfg(target_os = "macos")]
    {
        let path = plist_path().context("no home directory")?;
        if on {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let plist = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string><string>run</string></array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
                xml_escape(&exe.to_string_lossy())
            );
            std::fs::write(&path, plist)?;
        } else if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }
    #[cfg(windows)]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let status = if on {
            let cmd = format!("\"{}\" run", exe.display());
            std::process::Command::new("reg").args(["add", key, "/v", "PowerMac", "/t", "REG_SZ", "/d", &cmd, "/f"]).status()?
        } else {
            std::process::Command::new("reg").args(["delete", key, "/v", "PowerMac", "/f"]).status()?
        };
        anyhow::ensure!(status.success() || !on, "reg.exe failed");
    }
    let _ = exe;
    log::info!("launch at login: {}", if on { "on" } else { "off" });
    Ok(())
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
