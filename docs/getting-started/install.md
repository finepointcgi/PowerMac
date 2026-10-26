# Install and first run

## Requirements

| | macOS | Windows |
|---|---|---|
| OS | macOS 11 Big Sur or later (some modules need newer, noted on their pages) | Windows 10/11 (compile-checked only) |
| CPU | Apple Silicon recommended; Intel works except DDC/CI monitor control | x86_64 |
| Toolchain | Rust stable (`rustup`), Xcode Command Line Tools | Rust stable, MSVC build tools |

## Build the app bundle (macOS)

```bash
./scripts/bundle-macos.sh
```

The script builds `powermac` and the battery helper (`powermac-battery-helper`) in release mode and wraps them in `target/release/PowerMac.app` (it honours `CARGO_TARGET_DIR`). The app is menu-bar only: `LSUIElement` is set, so it has no Dock icon. Move it to `/Applications` and open it:

```bash
mv target/release/PowerMac.app /Applications/
open /Applications/PowerMac.app
```

### Keeping permissions across rebuilds

macOS ties Accessibility, Input Monitoring and Screen Recording grants to the app's code signature. An ad-hoc signature changes on every build, so the grants reset each time. To avoid that, create a stable local signing identity once:

```bash
./scripts/create-signing-cert.sh
```

This adds a self-signed **PowerMac Local Signing** certificate to your login keychain (macOS asks for your password). From then on `bundle-macos.sh` signs with it and the grants survive rebuilds.

!!! note "Distribution builds"
    The local certificate is for development. Builds for other people need a Developer ID certificate and notarization; see [Production readiness](../production-readiness.md).

## Run from the command line

```bash
cargo build --release
./target/release/powermac            # all enabled modules + tray icon
```

When run from a terminal, macOS attributes permissions to the terminal app, not to PowerMac. Use the bundle for everyday use. The [command line reference](cli.md) lists every subcommand.

## First launch

1. PowerMac creates `config.json` with defaults and a fresh **Mouse Without Borders security key** (see [Configuration files](configuration.md)).
2. A menu-bar icon appears. Its menu has **Settings…**, Awake modes, FancyZones layouts, scroll direction, mouse utilities, Paste as plain text, Copy security key, Launch at login, and a **Tools** section (Pick color, Extract text, Clipboard history, Window switcher, Show shelf, Displays presets, Finder Quick Actions).
3. Modules whose permission is missing log a message and stay off; the rest keep running. Open **Settings → General** to see each permission's status, with buttons that open the right System Settings pane. [Permissions](permissions.md) explains which module needs what.
4. Run `powermac doctor` at any time to print the config location, permission status and detected screens.

## Launch at login

Turn it on from the tray (**Launch at login**) or **Settings → General**.

## Logs

When PowerMac isn't attached to a terminal it logs to:

- macOS: `~/Library/Logs/PowerMac/powermac.log`
- Windows: `%LOCALAPPDATA%\PowerMac\powermac.log`

The log rotates to `powermac.log.old` at 5 MB. Set `RUST_LOG=debug` for more detail.

## Uninstall

1. Quit PowerMac from its menu.
2. If you installed the Finder Quick Actions, remove them in **Settings → File Tools** (or delete the `PowerMac — …` workflows in `~/Library/Services`).
3. If you installed the battery helper, uninstall it from **Settings → Battery** (it removes the LaunchDaemon `com.powermac.battery-helper`).
4. Delete the app and, to remove all settings and history, `~/Library/Application Support/PowerMac/`.
5. Remove PowerMac from the Privacy & Security lists in System Settings.
