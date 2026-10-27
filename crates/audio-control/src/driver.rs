//! The **PowerMac Audio** virtual device driver (`crates/pm-audio-driver`):
//! install, uninstall, find, and configure it.
//!
//! The driver is a Core Audio HAL plug-in. Installing copies
//! `PowerMac Audio.driver` into `/Library/Audio/Plug-Ins/HAL` and restarts
//! `coreaudiod`, behind **one** administrator prompt
//! (`osascript … with administrator privileges`). Restarting Core Audio
//! stops all sound on the Mac for a few seconds, so callers must show
//! [`RESTART_WARNING`] and get an explicit yes before calling [`install`] or
//! [`uninstall`]; nothing here runs on its own.
//!
//! Once loaded, the device set (names, channel counts, sample rates) is
//! changed at runtime with [`write_config`] — no reinstall, no restart.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

pub use pm_audio_driver::spec::{
    mix_name, mix_uid, DeviceSpec, DriverConfig, BUNDLE_ID, BUNDLE_NAME, CHANNEL_COUNTS, HAL_DIR, MAIN_NAME, MAIN_UID,
    MAX_DEVICES, SAMPLE_RATES,
};

/// Shown (and confirmed) before installing.
pub const RESTART_WARNING: &str = "Installing the PowerMac Audio driver restarts Core Audio. All sound on this Mac stops for a few seconds, and apps that are playing or recording may need to be restarted. macOS asks for an administrator password once.";
/// Shown (and confirmed) before uninstalling.
pub const UNINSTALL_WARNING: &str = "Removing the PowerMac Audio driver restarts Core Audio. All sound on this Mac stops for a few seconds, and apps using a PowerMac device switch to another one. macOS asks for an administrator password once.";

/// `/Library/Audio/Plug-Ins/HAL/PowerMac Audio.driver`.
pub fn installed_path() -> PathBuf {
    Path::new(HAL_DIR).join(BUNDLE_NAME)
}

/// The installed driver's version, if it is installed.
pub fn installed_version() -> Option<String> {
    let p = installed_path();
    p.is_dir().then(|| bundle_version(&p).ok()).flatten()
}

/// Installed / bundled versions, for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverStatus {
    NotInstalled,
    Installed { version: String },
    /// Installed, but the copy shipped with PowerMac has another version.
    UpdateAvailable { installed: String, bundled: String },
}

pub fn status(bundled: Option<&Path>) -> DriverStatus {
    let Some(installed) = installed_version() else { return DriverStatus::NotInstalled };
    match bundled.and_then(|b| bundle_version(b).ok()) {
        Some(b) if b != installed => DriverStatus::UpdateAvailable { installed, bundled: b },
        _ => DriverStatus::Installed { version: installed },
    }
}

/// The driver bundle shipped with this build: `PowerMac.app/Contents/Resources`,
/// or next to the executable (`target/release` after
/// `scripts/build-audio-driver.sh`).
pub fn find_bundled_driver() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates = vec![dir.join("../Resources").join(BUNDLE_NAME), dir.join(BUNDLE_NAME)];
    // target/debug/powermac → target/release/PowerMac Audio.driver
    if let Some(target) = dir.parent() {
        candidates.push(target.join("release").join(BUNDLE_NAME));
    }
    candidates.into_iter().find(|p| validate_bundle(p).is_ok()).map(|p| p.canonicalize().unwrap_or(p))
}

/// Value of a top-level string key in an XML Info.plist.
fn plist_string(xml: &str, key: &str) -> Option<String> {
    let k = format!("<key>{key}</key>");
    let rest = &xml[xml.find(&k)? + k.len()..];
    let rest = rest.trim_start();
    let body = rest.strip_prefix("<string>")?;
    let end = body.find("</string>")?;
    Some(body[..end].trim().to_string())
}

fn info_plist_value(bundle: &Path, key: &str) -> Result<String> {
    let path = bundle.join("Contents/Info.plist");
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    if let Some(v) = std::str::from_utf8(&bytes).ok().and_then(|xml| plist_string(xml, key)) {
        return Ok(v);
    }
    // Binary plist: let plutil read it.
    let out = std::process::Command::new("/usr/bin/plutil").args(["-extract", key, "raw", "-o", "-"]).arg(&path).output()?;
    if !out.status.success() {
        bail!("{key} missing from {}", path.display());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `CFBundleShortVersionString` of a driver bundle.
pub fn bundle_version(bundle: &Path) -> Result<String> {
    info_plist_value(bundle, "CFBundleShortVersionString")
}

/// Check that `bundle` is a PowerMac Audio driver bundle; returns its
/// version.
pub fn validate_bundle(bundle: &Path) -> Result<String> {
    if bundle.extension().and_then(|e| e.to_str()) != Some("driver") || !bundle.is_dir() {
        bail!("{} is not a .driver bundle", bundle.display());
    }
    let id = info_plist_value(bundle, "CFBundleIdentifier")?;
    if id != BUNDLE_ID {
        bail!("{} is {id}, not the PowerMac Audio driver", bundle.display());
    }
    let exe = info_plist_value(bundle, "CFBundleExecutable")?;
    if !bundle.join("Contents/MacOS").join(&exe).is_file() {
        bail!("{} has no executable", bundle.display());
    }
    bundle_version(bundle)
}

/// Single-quote for /bin/sh.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Escape for an AppleScript string literal.
pub fn applescript_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

/// The root shell script [`install`] runs: replace the bundle in the HAL
/// folder, owned by root, then restart coreaudiod (launchd starts it again
/// and it loads the plug-in).
pub fn install_script(bundle: &Path) -> String {
    let dst = sh_quote(&installed_path().to_string_lossy());
    format!(
        "set -e; mkdir -p {hal}; rm -rf {dst}; /usr/bin/ditto {src} {dst}; chown -R root:wheel {dst}; chmod -R go-w {dst}; \
         /usr/bin/xattr -dr com.apple.quarantine {dst} 2>/dev/null || true; killall coreaudiod 2>/dev/null || true",
        hal = sh_quote(HAL_DIR),
        src = sh_quote(&bundle.to_string_lossy()),
    )
}

/// The root shell script [`uninstall`] runs.
pub fn uninstall_script() -> String {
    format!("set -e; rm -rf {}; killall coreaudiod 2>/dev/null || true", sh_quote(&installed_path().to_string_lossy()))
}

/// The `osascript -e` argument for running `script` as root with one
/// password prompt.
pub fn admin_applescript(script: &str, prompt: &str) -> String {
    format!("do shell script {} with prompt {} with administrator privileges", applescript_quote(script), applescript_quote(prompt))
}

#[cfg(target_os = "macos")]
fn run_admin(script: &str, prompt: &str) -> Result<()> {
    let out = std::process::Command::new("/usr/bin/osascript").arg("-e").arg(admin_applescript(script, prompt)).output()?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("-128") {
        bail!("cancelled at the administrator prompt");
    }
    Err(anyhow!("{}", err.trim()))
}

#[cfg(not(target_os = "macos"))]
fn run_admin(_script: &str, _prompt: &str) -> Result<()> {
    bail!("the PowerMac Audio driver is macOS-only")
}

/// Install (or replace) the driver from `bundle_path` and restart Core
/// Audio. **Interrupts all audio**: show [`RESTART_WARNING`] and ask first.
/// Blocks while the administrator prompt is up. Returns the installed
/// version.
pub fn install(bundle_path: &Path) -> Result<String> {
    let version = validate_bundle(bundle_path)?;
    let src = bundle_path.canonicalize().unwrap_or_else(|_| bundle_path.to_path_buf());
    run_admin(&install_script(&src), "PowerMac wants to install the PowerMac Audio driver.")?;
    match installed_version() {
        Some(v) if v == version => Ok(v),
        Some(v) => Err(anyhow!("installed version is {v}, expected {version}")),
        None => Err(anyhow!("the driver was not installed")),
    }
}

/// Remove the driver and restart Core Audio. **Interrupts all audio**: show
/// [`UNINSTALL_WARNING`] and ask first.
pub fn uninstall() -> Result<()> {
    if !installed_path().exists() {
        return Ok(());
    }
    run_admin(&uninstall_script(), "PowerMac wants to remove the PowerMac Audio driver.")?;
    if installed_path().exists() {
        bail!("the driver is still installed");
    }
    Ok(())
}

/// Whether Core Audio has loaded the driver (its plug-in object exists).
pub fn loaded() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::plugin_object().is_some()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The running driver's configuration.
pub fn read_config() -> Result<DriverConfig> {
    #[cfg(target_os = "macos")]
    {
        mac::read_config()
    }
    #[cfg(not(target_os = "macos"))]
    {
        bail!("the PowerMac Audio driver is macOS-only")
    }
}

/// Change the running driver's devices (validated first). Adding, removing
/// and renaming devices take effect at once; channel-count and sample-rate
/// changes briefly stop IO on that device only. The driver persists it.
pub fn write_config(cfg: &DriverConfig) -> Result<()> {
    cfg.validate().map_err(|e| anyhow!(e))?;
    #[cfg(target_os = "macos")]
    {
        mac::write_config(cfg)
    }
    #[cfg(not(target_os = "macos"))]
    {
        bail!("the PowerMac Audio driver is macOS-only")
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use crate::macos::ffi::*;
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use pm_audio_driver::spec::{fourcc, CONFIG_PROPERTY};
    use std::ffi::c_void;

    const TRANSLATE_BUNDLE_ID_TO_PLUGIN: u32 = fourcc(b"bidp");

    pub fn plugin_object() -> Option<AudioObjectID> {
        let id = CFString::new(BUNDLE_ID);
        let r = id.as_concrete_TypeRef();
        let addr = AudioObjectPropertyAddress::global(TRANSLATE_BUNDLE_ID_TO_PLUGIN);
        let mut out: AudioObjectID = kAudioObjectUnknown;
        let mut size = 4u32;
        let st = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject,
                &addr,
                std::mem::size_of::<CFStringRef>() as u32,
                &r as *const CFStringRef as *const c_void,
                &mut size,
                &mut out as *mut AudioObjectID as *mut c_void,
            )
        };
        (st == 0 && out != kAudioObjectUnknown).then_some(out)
    }

    fn plugin() -> Result<AudioObjectID> {
        plugin_object().ok_or_else(|| anyhow!("the PowerMac Audio driver is not loaded (install it first)"))
    }

    pub fn read_config() -> Result<DriverConfig> {
        let json = crate::macos::props::get_string(plugin()?, AudioObjectPropertyAddress::global(CONFIG_PROPERTY))?;
        DriverConfig::from_json(&json).map_err(|e| anyhow!(e))
    }

    pub fn write_config(cfg: &DriverConfig) -> Result<()> {
        let s = CFString::new(&cfg.to_json());
        let r = s.as_concrete_TypeRef();
        let st = unsafe {
            AudioObjectSetPropertyData(
                plugin()?,
                &AudioObjectPropertyAddress::global(CONFIG_PROPERTY),
                0,
                std::ptr::null(),
                std::mem::size_of::<CFStringRef>() as u32,
                &r as *const CFStringRef as *const c_void,
            )
        };
        check(st, "setting the PowerMac Audio configuration")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(applescript_quote(r#"say "hi" \ bye"#), r#""say \"hi\" \\ bye""#);
    }

    #[test]
    fn install_script_quotes_paths_and_restarts_coreaudiod() {
        let s = install_script(Path::new("/tmp/my 'odd' dir/PowerMac Audio.driver"));
        assert!(s.contains(r"'/tmp/my '\''odd'\'' dir/PowerMac Audio.driver'"), "{s}");
        assert!(s.contains("'/Library/Audio/Plug-Ins/HAL/PowerMac Audio.driver'"));
        assert!(s.contains("chown -R root:wheel"));
        assert!(s.trim_end().ends_with("killall coreaudiod 2>/dev/null || true"));
        let u = uninstall_script();
        assert!(u.starts_with("set -e; rm -rf '/Library/Audio/Plug-Ins/HAL/PowerMac Audio.driver'"));
        assert!(u.contains("killall coreaudiod"));
    }

    #[test]
    fn admin_applescript_is_one_prompt() {
        let a = admin_applescript(&install_script(Path::new("/x/PowerMac Audio.driver")), "Install?");
        assert!(a.starts_with("do shell script \"set -e;"));
        assert!(a.ends_with("with prompt \"Install?\" with administrator privileges"));
        assert_eq!(a.matches("with administrator privileges").count(), 1);
    }

    #[test]
    fn plist_parsing() {
        let xml = pm_audio_driver::spec::info_plist("9.8.7");
        assert_eq!(plist_string(&xml, "CFBundleShortVersionString").as_deref(), Some("9.8.7"));
        assert_eq!(plist_string(&xml, "CFBundleIdentifier").as_deref(), Some(BUNDLE_ID));
        assert_eq!(plist_string(&xml, "Nope"), None);
    }

    #[test]
    fn validate_a_bundle_on_disk() {
        let dir = std::env::temp_dir().join(format!("pm-driver-test-{}", std::process::id()));
        let b = dir.join(BUNDLE_NAME);
        std::fs::create_dir_all(b.join("Contents/MacOS")).unwrap();
        std::fs::write(b.join("Contents/Info.plist"), pm_audio_driver::spec::info_plist("2.0.0")).unwrap();
        assert!(validate_bundle(&b).is_err(), "no executable yet");
        std::fs::write(b.join("Contents/MacOS/PowerMac Audio"), b"x").unwrap();
        assert_eq!(validate_bundle(&b).unwrap(), "2.0.0");
        assert!(validate_bundle(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_queries_are_read_only_and_consistent() {
        // Never installs anything: only reads the HAL folder and asks Core
        // Audio whether the plug-in is loaded.
        let st = status(None);
        assert_eq!(st == DriverStatus::NotInstalled, installed_version().is_none());
        if !loaded() {
            assert!(read_config().is_err());
        }
    }

    #[test]
    fn write_config_validates_first() {
        let mut c = DriverConfig::default();
        c.devices[0].channels = 5;
        assert!(write_config(&c).unwrap_err().to_string().contains("channels"));
    }
}
