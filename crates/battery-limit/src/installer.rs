//! Installing the privileged helper as a LaunchDaemon.
//!
//! The plist and the shell scripts are produced by pure functions (tested
//! below); [`install_helper`] / [`uninstall_helper`] run them through
//! `osascript … with administrator privileges`, i.e. one standard macOS admin
//! password prompt.

use std::path::Path;

/// launchd label, also the installed binary's file name.
pub const LABEL: &str = "com.powermac.battery-helper";
/// Where the helper listens when installed.
pub const SOCKET_PATH: &str = "/var/run/com.powermac.battery-helper.sock";

pub const HELPER_INSTALL_PATH: &str = "/Library/PrivilegedHelperTools/com.powermac.battery-helper";
pub const PLIST_PATH: &str = "/Library/LaunchDaemons/com.powermac.battery-helper.plist";
pub const LOG_PATH: &str = "/var/log/com.powermac.battery-helper.log";

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// POSIX shell single-quoting.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Escape for an AppleScript string literal.
pub fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The LaunchDaemon plist. `allowed_uid` is the only (non-root) user whose
/// processes may talk to the helper.
pub fn launchd_plist(allowed_uid: u32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{bin}</string>
		<string>--socket</string>
		<string>{socket}</string>
		<string>--allowed-uid</string>
		<string>{uid}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(LABEL),
        bin = xml_escape(HELPER_INSTALL_PATH),
        socket = xml_escape(SOCKET_PATH),
        uid = allowed_uid,
        log = xml_escape(LOG_PATH),
    )
}

/// Shell script (run as root) that installs `helper_src` and the plist
/// staged at `plist_src`, then (re)starts the daemon.
pub fn install_script(helper_src: &Path, plist_src: &Path) -> String {
    let src = shell_quote(&helper_src.to_string_lossy());
    let plist = shell_quote(&plist_src.to_string_lossy());
    [
        "set -e".to_string(),
        format!("/bin/launchctl bootout system/{LABEL} 2>/dev/null || true"),
        "/bin/mkdir -p /Library/PrivilegedHelperTools".to_string(),
        "/usr/sbin/chown root:wheel /Library/PrivilegedHelperTools".to_string(),
        format!("/usr/bin/install -m 0755 -o root -g wheel {src} {HELPER_INSTALL_PATH}"),
        format!("/usr/bin/plutil -lint {plist} >/dev/null"),
        format!("/usr/bin/install -m 0644 -o root -g wheel {plist} {PLIST_PATH}"),
        format!("/bin/launchctl bootstrap system {PLIST_PATH}"),
    ]
    .join("\n")
}

/// Shell script (run as root) that stops and removes the helper. Stopping
/// sends SIGTERM, on which the helper restores default charging.
pub fn uninstall_script() -> String {
    [
        format!("/bin/launchctl bootout system/{LABEL} 2>/dev/null || true"),
        format!("/bin/rm -f {PLIST_PATH} {HELPER_INSTALL_PATH} {SOCKET_PATH}"),
    ]
    .join("\n")
}

/// The `osascript -e` argument that runs `script` as root after one admin
/// prompt.
pub fn admin_applescript(script: &str, prompt: &str) -> String {
    format!(
        "do shell script {} with prompt {} with administrator privileges",
        applescript_string(script),
        applescript_string(prompt)
    )
}

/// Both the helper binary and the plist are in place.
pub fn helper_installed() -> bool {
    cfg!(target_os = "macos")
        && Path::new(HELPER_INSTALL_PATH).is_file()
        && Path::new(PLIST_PATH).is_file()
}

#[cfg(target_os = "macos")]
fn run_admin(script: &str, prompt: &str) -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    let out = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(admin_applescript(script, prompt))
        .output()
        .context("running osascript")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("-128") {
            bail!("cancelled");
        }
        bail!("{}", err.trim());
    }
    Ok(())
}

/// Copy `helper_binary` to /Library/PrivilegedHelperTools, write the
/// LaunchDaemon plist and bootstrap it. Shows one admin prompt.
#[cfg(target_os = "macos")]
pub fn install_helper(helper_binary: &Path) -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    if !helper_binary.is_file() {
        bail!("helper binary not found at {}", helper_binary.display());
    }
    // SAFETY: getuid never fails.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::temp_dir().join(format!("powermac-battery-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let plist = dir.join(format!("{LABEL}.plist"));
    std::fs::write(&plist, launchd_plist(uid)).context("staging plist")?;
    let r = run_admin(
        &install_script(helper_binary, &plist),
        "PowerMac wants to install a helper that can pause battery charging.",
    );
    let _ = std::fs::remove_dir_all(&dir);
    r
}

/// Stop and remove the helper (charging is restored as it exits).
#[cfg(target_os = "macos")]
pub fn uninstall_helper() -> anyhow::Result<()> {
    run_admin(
        &uninstall_script(),
        "PowerMac wants to remove its battery helper.",
    )
}

#[cfg(not(target_os = "macos"))]
pub fn install_helper(_helper_binary: &Path) -> anyhow::Result<()> {
    anyhow::bail!("the battery helper is macOS-only")
}

#[cfg(not(target_os = "macos"))]
pub fn uninstall_helper() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contents() {
        let p = launchd_plist(501);
        assert!(p.contains("<string>com.powermac.battery-helper</string>"));
        assert!(p.contains("<string>--allowed-uid</string>\n\t\t<string>501</string>"));
        assert!(p.contains(HELPER_INSTALL_PATH));
        assert!(p.contains("<key>KeepAlive</key>\n\t<true/>"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_lints() {
        let dir = std::env::temp_dir().join(format!("bl-plist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.plist");
        std::fs::write(&f, launchd_plist(501)).unwrap();
        let out = std::process::Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&f)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let out = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", "ProgramArguments.4", "raw", "-o", "-"])
            .arg(&f)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "501");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(
            applescript_string(r#"say "hi" \ ok"#),
            r#""say \"hi\" \\ ok""#
        );
    }

    #[test]
    fn install_script_shape() {
        let s = install_script(
            Path::new("/Applications/Power Mac's.app/Contents/MacOS/powermac-battery-helper"),
            Path::new("/var/folders/x/T/powermac-battery-1/com.powermac.battery-helper.plist"),
        );
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "set -e");
        assert!(lines[1].starts_with("/bin/launchctl bootout system/com.powermac.battery-helper"));
        assert!(s.contains(
            r"/usr/bin/install -m 0755 -o root -g wheel '/Applications/Power Mac'\''s.app/Contents/MacOS/powermac-battery-helper' /Library/PrivilegedHelperTools/com.powermac.battery-helper"
        ));
        assert!(s.contains("/usr/bin/install -m 0644 -o root -g wheel '/var/folders/x/T/powermac-battery-1/com.powermac.battery-helper.plist' /Library/LaunchDaemons/com.powermac.battery-helper.plist"));
        assert_eq!(*lines.last().unwrap(), "/bin/launchctl bootstrap system /Library/LaunchDaemons/com.powermac.battery-helper.plist");
        // Lint happens before the plist goes live.
        assert!(s.find("plutil -lint").unwrap() < s.find("0644").unwrap());
    }

    #[test]
    fn uninstall_script_shape() {
        let s = uninstall_script();
        assert!(s.starts_with("/bin/launchctl bootout system/com.powermac.battery-helper"));
        assert!(s.contains("/bin/rm -f /Library/LaunchDaemons/com.powermac.battery-helper.plist /Library/PrivilegedHelperTools/com.powermac.battery-helper /var/run/com.powermac.battery-helper.sock"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_compiles_and_shell_parses() {
        // Syntax-check only: `sh -n` parses without running, and osacompile
        // compiles the AppleScript without executing it.
        let script = install_script(Path::new("/tmp/a \"b\" c's"), Path::new("/tmp/p.plist"));
        let out = std::process::Command::new("/bin/sh")
            .arg("-n")
            .arg("-c")
            .arg(&script)
            .output()
            .unwrap();
        assert!(out.status.success());
        let dir = std::env::temp_dir().join(format!("bl-osa-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = std::process::Command::new("/usr/bin/osacompile")
            .arg("-o")
            .arg(dir.join("t.scpt"))
            .arg("-e")
            .arg(admin_applescript(&script, "prompt \"x\""))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
