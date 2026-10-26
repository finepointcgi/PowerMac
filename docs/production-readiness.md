# Production readiness

What stands between the current 0.1.0 and a release other people can install.

## Known gaps

| Area | Gap |
|---|---|
| **Windows** | Every crate is compile-checked (`cargo check --target x86_64-pc-windows-msvc`) but none has been run on Windows. |
| **Mouse Without Borders** | No file transfer, drag-and-drop or clipboard images. About 1 in 512 keys trips a C# quirk in Windows MWB; PowerMac warns, generate a new key. |
| **Scrolling on Windows** | Can't tell a touchpad from a mouse; an optional heuristic exists. |
| **Keyboard Manager** | No two-step chords, no PowerToys "alone" remaps; CapsLock as a tap only on macOS. |
| **FancyZones on macOS** | Dragged windows can't be made see-through; zone assignments are lost on restart. |
| **Displays** | DDC/CI on Apple Silicon only. |
| **Other snappers** | Rectangle or macOS tiling must be off to avoid double snapping. |
| **Private APIs** | Several modules use private macOS APIs (CGS Spaces, `CGVirtualDisplay`, `IOAVService`, DisplayServices, `CoreDockSendNotification`, SMC). They can break with OS updates and rule out the Mac App Store. |
| **Not ported** | Mouse Jump, Always On Top, Workspaces. |

## Release checklist

### Distribution (macOS)

- [ ] Developer ID Application certificate; sign with hardened runtime (`codesign --options runtime --timestamp`).
- [x] Embed the battery helper in the bundle and sign it separately (`bundle-macos.sh` does this; switch it to the Developer ID).
- [ ] Notarize (`xcrun notarytool submit --wait`) and staple (`xcrun stapler staple`).
- [ ] Ship as a signed, notarized `.dmg` or `.zip`.
- [ ] A real bundle identifier you own instead of `com.powermac.app`, plus an app icon.
- [x] Usage-description strings in `Info.plist` for Calendars, Camera, Automation (Apple Events), Audio capture.
- [ ] Auto-update (e.g. Sparkle) or at least an update check.

### Distribution (Windows)

- [ ] Run every module on Windows 10 and 11; fix what fails.
- [ ] Authenticode-signed installer (MSI/MSIX) with start-at-login.

### Quality

- [ ] CI running `cargo fmt --check`, `cargo clippy --workspace -D warnings`, `cargo test --workspace` on macOS and `cargo check` for Windows.
- [ ] A written live-test checklist per module (the [Building and testing](development/building.md#live-checks) list as a starting point) run before each release.
- [ ] Crash reporting or at least panic logging to the log file.
- [ ] Persist FancyZones zone assignments across restarts.

### Safety

- [ ] Review every module's `stop` path once more: pointer settings, gamma, virtual displays, `hidutil` mappings, audio taps, SMC state.
- [ ] Security review of the battery helper's socket protocol and installer.
- [ ] Security review of MWB: listen address, discovery scope, key handling.
- [ ] Clipboard history: confirm excluded apps and concealed types on current macOS.

### Documentation

- [ ] Screenshots of each settings page on its feature page (`PM_UI_SHOTS` renders them).
- [ ] A CHANGELOG and versioned release notes.
- [ ] Privacy statement: what is stored locally, the only network calls (MWB on the LAN, Open-Meteo for the weather widget).
