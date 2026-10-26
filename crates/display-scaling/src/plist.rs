//! Display override plists (the "one-key-hidpi" way): a file at
//! `/Library/Displays/Contents/Resources/Overrides/DisplayVendorID-<hex>/DisplayProductID-<hex>`
//! whose `scale-resolutions` make macOS offer HiDPI modes for a non-Retina
//! display without a virtual display. Installing needs administrator rights
//! (the user is asked by macOS) and takes effect after the display is
//! reconnected or the Mac restarts.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::Resolution;

pub const OVERRIDES_DIR: &str = "/Library/Displays/Contents/Resources/Overrides";

/// A generated override: where it goes and what it contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverridePlist {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// `/Library/Displays/.../DisplayVendorID-<vendor>/DisplayProductID-<product>`
/// (lowercase hex without leading zeros, as macOS names them).
pub fn override_path(vendor: u32, product: u32) -> PathBuf {
    Path::new(OVERRIDES_DIR).join(format!("DisplayVendorID-{vendor:x}")).join(format!("DisplayProductID-{product:x}"))
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

fn xml_escape(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&apos;".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// One `scale-resolutions` entry: the backing size (2x the logical size),
/// big-endian, followed by the flags macOS uses for HiDPI modes
/// (`0x00000001 0x00200000`).
pub fn scale_resolution_entry(logical: Resolution) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0..4].copy_from_slice(&(logical.width * 2).to_be_bytes());
    b[4..8].copy_from_slice(&(logical.height * 2).to_be_bytes());
    b[8..12].copy_from_slice(&1u32.to_be_bytes());
    b[12..16].copy_from_slice(&0x0020_0000u32.to_be_bytes());
    b
}

/// The override plist for a display (`vendor` / `product` are what
/// `DisplayModes::vendor` / `::model` report) offering `logical` as HiDPI.
pub fn generate_override(vendor: u32, product: u32, name: &str, logical: &[Resolution]) -> OverridePlist {
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    x.push_str("<plist version=\"1.0\">\n<dict>\n");
    let _ = writeln!(x, "\t<key>DisplayProductName</key>\n\t<string>{}</string>", xml_escape(name));
    let _ = writeln!(x, "\t<key>DisplayProductID</key>\n\t<integer>{product}</integer>");
    let _ = writeln!(x, "\t<key>DisplayVendorID</key>\n\t<integer>{vendor}</integer>");
    x.push_str("\t<key>scale-resolutions</key>\n\t<array>\n");
    let mut seen = Vec::new();
    for r in logical {
        if r.width == 0 || r.height == 0 || seen.contains(r) {
            continue;
        }
        seen.push(*r);
        let _ = writeln!(x, "\t\t<data>{}</data>", base64(&scale_resolution_entry(*r)));
    }
    x.push_str("\t</array>\n");
    // Lets macOS pick a sensible default scale on the panel.
    x.push_str("\t<key>target-default-ppmm</key>\n\t<real>10.0699301</real>\n");
    x.push_str("</dict>\n</plist>\n");
    OverridePlist { path: override_path(vendor, product), bytes: x.into_bytes() }
}

/// Single-quote for /bin/sh.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Escape for an AppleScript string literal.
pub fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The shell script run as root to install `staged` at `target`.
pub fn install_script(staged: &Path, target: &Path) -> String {
    let dir = target.parent().unwrap_or(Path::new(OVERRIDES_DIR));
    format!(
        "/bin/mkdir -p {d} && /bin/cp {s} {t} && /usr/sbin/chown root:wheel {t} && /bin/chmod 644 {t}",
        d = shell_quote(&dir.to_string_lossy()),
        s = shell_quote(&staged.to_string_lossy()),
        t = shell_quote(&target.to_string_lossy())
    )
}

/// The shell script run as root to remove an override.
pub fn remove_script(target: &Path) -> String {
    format!("/bin/rm -f {}", shell_quote(&target.to_string_lossy()))
}

/// `osascript -e <this>`: run `script` as root after macOS asks for an
/// administrator password.
pub fn admin_applescript(script: &str) -> String {
    format!("do shell script {} with administrator privileges", applescript_string(script))
}

#[cfg(target_os = "macos")]
fn run_admin(script: &str) -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    let out = std::process::Command::new("/usr/bin/osascript").arg("-e").arg(admin_applescript(script)).output().context("running osascript")?;
    if !out.status.success() {
        bail!("osascript failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Install the override (macOS asks for an administrator password). Takes
/// effect after the display is reconnected or the Mac restarts.
#[cfg(target_os = "macos")]
pub fn install_override_with_admin(o: &OverridePlist) -> anyhow::Result<()> {
    use anyhow::Context;
    let staged = std::env::temp_dir().join(format!("powermac-display-override-{}.plist", std::process::id()));
    std::fs::write(&staged, &o.bytes).with_context(|| format!("writing {}", staged.display()))?;
    let r = run_admin(&install_script(&staged, &o.path));
    let _ = std::fs::remove_file(&staged);
    r
}

/// Remove an installed override (asks for an administrator password).
#[cfg(target_os = "macos")]
pub fn remove_override_with_admin(vendor: u32, product: u32) -> anyhow::Result<()> {
    run_admin(&remove_script(&override_path(vendor, product)))
}

#[cfg(not(target_os = "macos"))]
pub fn install_override_with_admin(_: &OverridePlist) -> anyhow::Result<()> {
    anyhow::bail!("display overrides are macOS-only")
}

#[cfg(not(target_os = "macos"))]
pub fn remove_override_with_admin(_: u32, _: u32) -> anyhow::Result<()> {
    anyhow::bail!("display overrides are macOS-only")
}

/// Whether an override is installed for a display.
pub fn override_installed(vendor: u32, product: u32) -> bool {
    override_path(vendor, product).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_entries() {
        assert_eq!(
            override_path(0x1e6d, 0x5b7f),
            PathBuf::from("/Library/Displays/Contents/Resources/Overrides/DisplayVendorID-1e6d/DisplayProductID-5b7f")
        );
        assert_eq!(override_path(0x10ac, 0xa0c4).file_name().unwrap(), "DisplayProductID-a0c4");
        // one-key-hidpi's well-known 1920x1080 HiDPI entry
        assert_eq!(base64(&scale_resolution_entry(Resolution { width: 1920, height: 1080 })), "AAAPAAAACHAAAAABACAAAA==");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    #[test]
    fn plist_content() {
        let o = generate_override(0x1e6d, 0x5b7f, "LG <QHD> & \"x\"", &[Resolution { width: 1920, height: 1080 }, Resolution { width: 1920, height: 1080 }, Resolution { width: 2560, height: 1440 }]);
        let s = String::from_utf8(o.bytes.clone()).unwrap();
        assert!(s.contains("<string>LG &lt;QHD&gt; &amp; &quot;x&quot;</string>"));
        assert!(s.contains("<integer>7789</integer>") && s.contains("<integer>23423</integer>"));
        assert_eq!(s.matches("<data>").count(), 2);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_lints() {
        let dir = std::env::temp_dir().join(format!("display-scaling-plist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let o = generate_override(0x10ac, 0xa0c4, "DELL U2719D", &crate::logic::hidpi_logical_resolutions(Resolution { width: 2560, height: 1440 }, &[]));
        let f = dir.join("DisplayProductID-a0c4");
        std::fs::write(&f, &o.bytes).unwrap();
        let out = std::process::Command::new("/usr/bin/plutil").arg("-lint").arg(&f).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        let get = |key: &str| String::from_utf8_lossy(&std::process::Command::new("/usr/bin/plutil").args(["-extract", key, "raw", "-o", "-"]).arg(&f).output().unwrap().stdout).trim().to_string();
        assert_eq!(get("DisplayVendorID"), "4268");
        assert_eq!(get("DisplayProductID"), "41156");
        assert_eq!(get("scale-resolutions"), "12", "array count");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn admin_scripts_quote() {
        let s = install_script(Path::new("/tmp/a b'c.plist"), &override_path(1, 2));
        assert!(s.contains(r"'/tmp/a b'\''c.plist'"));
        assert!(s.starts_with("/bin/mkdir -p '/Library/Displays/Contents/Resources/Overrides/DisplayVendorID-1'"));
        let a = admin_applescript(r#"echo "hi" \ there"#);
        assert_eq!(a, r#"do shell script "echo \"hi\" \\ there" with administrator privileges"#);
        assert_eq!(remove_script(&override_path(1, 2)), "/bin/rm -f '/Library/Displays/Contents/Resources/Overrides/DisplayVendorID-1/DisplayProductID-2'");
    }
}
