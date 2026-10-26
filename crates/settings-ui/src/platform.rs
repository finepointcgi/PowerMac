//! Small OS helpers: permission status, opening folders / System Settings,
//! and making the settings process a normal foreground app on macOS.

use std::path::Path;

#[cfg(target_os = "macos")]
mod mac {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn AXIsProcessTrusted() -> bool;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGPreflightListenEventAccess() -> bool;
    }
}

/// (Accessibility, Input Monitoring) on macOS; `None` elsewhere.
pub fn permissions() -> Option<(bool, bool)> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: plain C functions without arguments.
        unsafe { Some((mac::AXIsProcessTrusted(), mac::CGPreflightListenEventAccess())) }
    }
    #[cfg(not(target_os = "macos"))]
    None
}

/// Open System Settings → Privacy & Security at `pane`
/// (`Privacy_Accessibility` or `Privacy_ListenEvent`).
pub fn open_privacy_pane(pane: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg(format!("x-apple.systempreferences:com.apple.preference.security?{pane}"))
        .spawn();
    #[cfg(not(target_os = "macos"))]
    let _ = pane;
}

pub fn open_folder(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
}

/// Show a Dock icon while the window is open and come to the front (the
/// app bundle is `LSUIElement`, which would otherwise hide both).
pub fn become_foreground_app() {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        let Some(mtm) = objc2::MainThreadMarker::new() else { return };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

/// 64x64 app icon: a screen split into zones (like the tray icon), in blue.
pub fn icon() -> eframe::egui::IconData {
    const N: usize = 64;
    let mut px = vec![0u8; N * N * 4];
    let mut set = |x: usize, y: usize, c: [u8; 4]| {
        let i = (y * N + x) * 4;
        px[i..i + 4].copy_from_slice(&c);
    };
    let blue = [0x2b, 0x88, 0xd8, 255];
    let light = [0xd6, 0xe9, 0xfa, 255];
    for y in 8..56 {
        for x in 4..60 {
            let border = !(8 + 3..56 - 3).contains(&y) || !(4 + 3..60 - 3).contains(&x);
            let divider = (29..32).contains(&x) || (x >= 29 && (31..34).contains(&y));
            let active = (12..52).contains(&y) && (11..26).contains(&x);
            set(x, y, if border || divider || active { blue } else { light });
        }
    }
    eframe::egui::IconData { rgba: px, width: N as u32, height: N as u32 }
}

/// Running regular (Dock) apps as (name, bundle id), sorted by name; empty
/// where this isn't available.
pub fn running_apps() -> Vec<(String, String)> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};
        let mut v: Vec<(String, String)> = NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .filter(|a| a.activationPolicy() == NSApplicationActivationPolicy::Regular)
            .filter_map(|a| {
                let id = a.bundleIdentifier()?.to_string();
                let name = a.localizedName().map(|n| n.to_string()).unwrap_or_else(|| id.clone());
                Some((name, id))
            })
            .collect();
        v.sort_by_key(|(n, _)| n.to_lowercase());
        v.dedup_by(|a, b| a.1 == b.1);
        v
    }
    #[cfg(not(target_os = "macos"))]
    Vec::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn running_apps_lists_bundle_ids() {
        for (name, id) in super::running_apps() {
            assert!(!name.is_empty() && !id.is_empty());
        }
    }
}
